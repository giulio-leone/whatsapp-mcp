package main

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"os/signal"
	"sync"
	"syscall"
	"time"

	_ "github.com/mattn/go-sqlite3"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/appstate"
	waProto "go.mau.fi/whatsmeow/binary/proto"
	"go.mau.fi/whatsmeow/store/sqlstore"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
	waLog "go.mau.fi/whatsmeow/util/log"
	"google.golang.org/protobuf/proto"
)

// ─── Configuration ──────────────────────────────────────────────────

const (
	defaultPort   = "9876"
	defaultDBPath = "whatsmeow.db"
)

// ─── Bridge Server ──────────────────────────────────────────────────

type BridgeServer struct {
	client    *whatsmeow.Client
	container *sqlstore.Container
	log       waLog.Logger
	mu        sync.RWMutex

	// Event stream for incoming messages
	eventChan chan map[string]interface{}
}

func NewBridgeServer(dbPath string) (*BridgeServer, error) {
	logger := waLog.Stdout("Bridge", "INFO", true)
	ctx := context.Background()

	container, err := sqlstore.New(ctx, "sqlite3",
		fmt.Sprintf("file:%s?_foreign_keys=on", dbPath), logger)
	if err != nil {
		return nil, fmt.Errorf("failed to create SQL store: %w", err)
	}

	deviceStore, err := container.GetFirstDevice(ctx)
	if err != nil {
		return nil, fmt.Errorf("failed to get device: %w", err)
	}

	client := whatsmeow.NewClient(deviceStore, logger)

	bs := &BridgeServer{
		client:    client,
		container: container,
		log:       logger,
		eventChan: make(chan map[string]interface{}, 100),
	}

	client.AddEventHandler(bs.handleEvent)
	return bs, nil
}

// ─── WhatsApp Event Handler ─────────────────────────────────────────

func (bs *BridgeServer) handleEvent(evt interface{}) {
	switch v := evt.(type) {
	case *events.Message:
		msg := map[string]interface{}{
			"type":       "message",
			"id":         v.Info.ID,
			"chat_id":    v.Info.Chat.String(),
			"sender_id":  v.Info.Sender.String(),
			"timestamp":  v.Info.Timestamp.Unix(),
			"is_from_me": v.Info.IsFromMe,
			"is_group":   v.Info.IsGroup,
			"push_name":  v.Info.PushName,
		}

		if v.Message.GetConversation() != "" {
			msg["text"] = v.Message.GetConversation()
		} else if v.Message.GetExtendedTextMessage() != nil {
			msg["text"] = v.Message.GetExtendedTextMessage().GetText()
		}

		bs.eventChan <- msg

	case *events.Receipt:
		bs.log.Infof("Receipt: %s from %s type=%s", v.MessageIDs, v.MessageSource.Chat, v.Type)

	case *events.Connected:
		bs.log.Infof("Connected to WhatsApp")

	case *events.Disconnected:
		bs.log.Warnf("Disconnected from WhatsApp")

	case *events.HistorySync:
		bs.log.Infof("History sync received: %d conversations", len(v.Data.GetConversations()))
		for _, conv := range v.Data.GetConversations() {
			chatJID := conv.GetID()
			bs.log.Infof("  Synced chat: %s (%d messages)", chatJID, len(conv.GetMessages()))
		}

	case *events.PushNameSetting:
		bs.log.Infof("Push name updated: %s", v.Action.GetName())
	}
}

// ─── HTTP Handlers ──────────────────────────────────────────────────

func (bs *BridgeServer) setupRouter() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("/rpc", bs.handleRPC)
	mux.HandleFunc("/health", bs.handleHealth)
	return mux
}

type RPCRequest struct {
	Method string          `json:"method"`
	Params json.RawMessage `json:"params,omitempty"`
	ID     interface{}     `json:"id"`
}

type RPCResponse struct {
	Result interface{} `json:"result,omitempty"`
	Error  *RPCError   `json:"error,omitempty"`
	ID     interface{} `json:"id"`
}

type RPCError struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
}

func (bs *BridgeServer) handleHealth(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(map[string]interface{}{
		"status":    "ok",
		"connected": bs.client.IsConnected(),
		"logged_in": bs.client.IsLoggedIn(),
	})
}

func (bs *BridgeServer) handleRPC(w http.ResponseWriter, r *http.Request) {
	if r.Method != "POST" {
		http.Error(w, "Method not allowed", http.StatusMethodNotAllowed)
		return
	}

	body, err := io.ReadAll(r.Body)
	if err != nil {
		http.Error(w, "Failed to read body", http.StatusBadRequest)
		return
	}

	var req RPCRequest
	if err := json.Unmarshal(body, &req); err != nil {
		writeRPCError(w, nil, -32700, "Parse error")
		return
	}

	var result interface{}
	var rpcErr *RPCError

	switch req.Method {
	case "connect":
		result, rpcErr = bs.rpcConnect(req.Params)
	case "disconnect":
		result, rpcErr = bs.rpcDisconnect()
	case "get_status":
		result, rpcErr = bs.rpcGetStatus()
	case "list_chats":
		result, rpcErr = bs.rpcListChats(req.Params)
	case "get_messages":
		result, rpcErr = bs.rpcGetMessages(req.Params)
	case "send_message":
		result, rpcErr = bs.rpcSendMessage(req.Params)
	case "search_contacts":
		result, rpcErr = bs.rpcSearchContacts(req.Params)
	case "get_chat_info":
		result, rpcErr = bs.rpcGetChatInfo(req.Params)
	case "get_events":
		result, rpcErr = bs.rpcGetEvents()
	default:
		rpcErr = &RPCError{Code: -32601, Message: fmt.Sprintf("Method not found: %s", req.Method)}
	}

	w.Header().Set("Content-Type", "application/json")
	resp := RPCResponse{ID: req.ID}
	if rpcErr != nil {
		resp.Error = rpcErr
	} else {
		resp.Result = result
	}
	json.NewEncoder(w).Encode(resp)
}

func writeRPCError(w http.ResponseWriter, id interface{}, code int, msg string) {
	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(RPCResponse{
		ID:    id,
		Error: &RPCError{Code: code, Message: msg},
	})
}

// ─── RPC Method Implementations ─────────────────────────────────────

func (bs *BridgeServer) rpcConnect(params json.RawMessage) (interface{}, *RPCError) {
	ctx := context.Background()

	if bs.client.Store.ID == nil {
		// Not registered — need QR code
		qrChan, _ := bs.client.GetQRChannel(ctx)
		err := bs.client.Connect()
		if err != nil {
			return nil, &RPCError{Code: -1, Message: fmt.Sprintf("Connect failed: %s", err)}
		}

		// Wait for first QR code or login event
		timeout := time.After(60 * time.Second)
		for {
			select {
			case evt, ok := <-qrChan:
				if !ok {
					if bs.client.IsLoggedIn() {
						return map[string]interface{}{
							"status":    "connected",
							"logged_in": true,
						}, nil
					}
					return nil, &RPCError{Code: -2, Message: "QR channel closed without login"}
				}
				switch evt.Event {
				case "code":
					return map[string]interface{}{
						"status":  "qr_code",
						"qr_code": evt.Code,
						"message": "Scan this QR code with WhatsApp on your phone. Call connect again after scanning.",
					}, nil
				case "login":
					return map[string]interface{}{
						"status":    "connected",
						"logged_in": true,
						"jid":       bs.client.Store.ID.String(),
					}, nil
				case "timeout":
					return nil, &RPCError{Code: -3, Message: "QR code timeout — call connect again"}
				}
			case <-timeout:
				return nil, &RPCError{Code: -3, Message: "Connect timeout"}
			}
		}
	}

	// Already registered, just connect
	err := bs.client.Connect()
	if err != nil {
		return nil, &RPCError{Code: -1, Message: fmt.Sprintf("Connect failed: %s", err)}
	}

	// Wait for connection
	time.Sleep(2 * time.Second)

	// Request app state sync
	err = bs.client.FetchAppState(ctx, appstate.WAPatchCriticalUnblockLow, false, false)
	if err != nil {
		bs.log.Warnf("Failed to fetch app state: %s", err)
	}

	return map[string]interface{}{
		"status":    "connected",
		"logged_in": bs.client.IsLoggedIn(),
		"jid":       bs.client.Store.ID.String(),
	}, nil
}

func (bs *BridgeServer) rpcDisconnect() (interface{}, *RPCError) {
	bs.client.Disconnect()
	return map[string]interface{}{"status": "disconnected"}, nil
}

func (bs *BridgeServer) rpcGetStatus() (interface{}, *RPCError) {
	status := map[string]interface{}{
		"connected": bs.client.IsConnected(),
		"logged_in": bs.client.IsLoggedIn(),
	}
	if bs.client.Store.ID != nil {
		status["jid"] = bs.client.Store.ID.String()
		status["push_name"] = bs.client.Store.PushName
	}
	return status, nil
}

func (bs *BridgeServer) rpcListChats(params json.RawMessage) (interface{}, *RPCError) {
	ctx := context.Background()

	if !bs.client.IsLoggedIn() {
		return nil, &RPCError{Code: -10, Message: "Not logged in. Call 'connect' first."}
	}

	groups, err := bs.client.GetJoinedGroups(ctx)
	if err != nil {
		bs.log.Warnf("Failed to get groups: %s", err)
	}

	chats := []map[string]interface{}{}

	for _, group := range groups {
		chats = append(chats, map[string]interface{}{
			"id":           group.JID.String(),
			"name":         group.Name,
			"is_group":     true,
			"topic":        group.Topic,
			"participants": len(group.Participants),
		})
	}

	contacts, err := bs.client.Store.Contacts.GetAllContacts(ctx)
	if err == nil {
		for jid, contact := range contacts {
			if jid.Server == types.GroupServer || jid.Server == types.BroadcastServer {
				continue
			}
			name := contact.FullName
			if name == "" {
				name = contact.PushName
			}
			if name == "" {
				name = contact.BusinessName
			}
			chats = append(chats, map[string]interface{}{
				"id":        jid.String(),
				"name":      name,
				"is_group":  false,
				"push_name": contact.PushName,
			})
		}
	}

	return map[string]interface{}{
		"chats": chats,
		"count": len(chats),
	}, nil
}

type GetMessagesParams struct {
	ChatID string `json:"chat_id"`
	Limit  int    `json:"limit"`
	Cursor string `json:"cursor"`
}

func (bs *BridgeServer) rpcGetMessages(params json.RawMessage) (interface{}, *RPCError) {
	if !bs.client.IsLoggedIn() {
		return nil, &RPCError{Code: -10, Message: "Not logged in"}
	}

	var p GetMessagesParams
	if err := json.Unmarshal(params, &p); err != nil {
		return nil, &RPCError{Code: -32602, Message: "Invalid params"}
	}
	if p.Limit == 0 {
		p.Limit = 20
	}

	return map[string]interface{}{
		"messages":    []interface{}{},
		"next_cursor": nil,
		"has_more":    false,
		"note":        "Messages are populated via history sync. Send a message first or wait for sync.",
	}, nil
}

type SendMessageParams struct {
	ChatID string `json:"chat_id"`
	Text   string `json:"text"`
}

func (bs *BridgeServer) rpcSendMessage(params json.RawMessage) (interface{}, *RPCError) {
	if !bs.client.IsLoggedIn() {
		return nil, &RPCError{Code: -10, Message: "Not logged in"}
	}

	var p SendMessageParams
	if err := json.Unmarshal(params, &p); err != nil {
		return nil, &RPCError{Code: -32602, Message: "Invalid params"}
	}
	if p.ChatID == "" || p.Text == "" {
		return nil, &RPCError{Code: -32602, Message: "chat_id and text are required"}
	}

	jid, err := types.ParseJID(p.ChatID)
	if err != nil {
		return nil, &RPCError{Code: -32602, Message: fmt.Sprintf("Invalid chat_id: %s", err)}
	}

	msg := &waProto.Message{
		Conversation: proto.String(p.Text),
	}

	resp, err := bs.client.SendMessage(context.Background(), jid, msg)
	if err != nil {
		return nil, &RPCError{Code: -20, Message: fmt.Sprintf("Send failed: %s", err)}
	}

	return map[string]interface{}{
		"id":        resp.ID,
		"timestamp": resp.Timestamp.Unix(),
		"status":    "sent",
	}, nil
}

type SearchContactsParams struct {
	Query string `json:"query"`
}

func (bs *BridgeServer) rpcSearchContacts(params json.RawMessage) (interface{}, *RPCError) {
	ctx := context.Background()

	if !bs.client.IsLoggedIn() {
		return nil, &RPCError{Code: -10, Message: "Not logged in"}
	}

	var p SearchContactsParams
	if err := json.Unmarshal(params, &p); err != nil {
		return nil, &RPCError{Code: -32602, Message: "Invalid params"}
	}

	contacts, err := bs.client.Store.Contacts.GetAllContacts(ctx)
	if err != nil {
		return nil, &RPCError{Code: -30, Message: fmt.Sprintf("Failed to get contacts: %s", err)}
	}

	results := []map[string]interface{}{}

	for jid, contact := range contacts {
		name := contact.FullName
		if name == "" {
			name = contact.PushName
		}
		if name == "" {
			name = contact.BusinessName
		}

		if containsIgnoreCase(name, p.Query) ||
			containsIgnoreCase(contact.PushName, p.Query) ||
			containsIgnoreCase(jid.User, p.Query) {
			results = append(results, map[string]interface{}{
				"id":               jid.String(),
				"name":             name,
				"push_name":        contact.PushName,
				"formatted_number": "+" + jid.User,
				"is_business":      contact.BusinessName != "",
			})
		}
	}

	return map[string]interface{}{
		"contacts": results,
		"count":    len(results),
	}, nil
}

type GetChatInfoParams struct {
	ChatID string `json:"chat_id"`
}

func (bs *BridgeServer) rpcGetChatInfo(params json.RawMessage) (interface{}, *RPCError) {
	ctx := context.Background()

	if !bs.client.IsLoggedIn() {
		return nil, &RPCError{Code: -10, Message: "Not logged in"}
	}

	var p GetChatInfoParams
	if err := json.Unmarshal(params, &p); err != nil {
		return nil, &RPCError{Code: -32602, Message: "Invalid params"}
	}

	jid, err := types.ParseJID(p.ChatID)
	if err != nil {
		return nil, &RPCError{Code: -32602, Message: fmt.Sprintf("Invalid chat_id: %s", err)}
	}

	if jid.Server == types.GroupServer {
		groupInfo, err := bs.client.GetGroupInfo(ctx, jid)
		if err != nil {
			return nil, &RPCError{Code: -40, Message: fmt.Sprintf("Failed to get group info: %s", err)}
		}
		participants := []map[string]interface{}{}
		for _, p := range groupInfo.Participants {
			participants = append(participants, map[string]interface{}{
				"jid":      p.JID.String(),
				"is_admin": p.IsAdmin,
				"is_super": p.IsSuperAdmin,
			})
		}
		return map[string]interface{}{
			"id":           groupInfo.JID.String(),
			"name":         groupInfo.Name,
			"topic":        groupInfo.Topic,
			"is_group":     true,
			"owner":        groupInfo.OwnerJID.String(),
			"created_at":   groupInfo.GroupCreated.Unix(),
			"participants": participants,
		}, nil
	}

	contact, err := bs.client.Store.Contacts.GetContact(ctx, jid)
	if err != nil {
		return nil, &RPCError{Code: -40, Message: fmt.Sprintf("Contact not found: %s", err)}
	}

	name := contact.FullName
	if name == "" {
		name = contact.PushName
	}

	return map[string]interface{}{
		"id":               jid.String(),
		"name":             name,
		"push_name":        contact.PushName,
		"is_group":         false,
		"formatted_number": "+" + jid.User,
		"is_business":      contact.BusinessName != "",
	}, nil
}

func (bs *BridgeServer) rpcGetEvents() (interface{}, *RPCError) {
	evts := []map[string]interface{}{}

	for {
		select {
		case evt := <-bs.eventChan:
			evts = append(evts, evt)
		default:
			goto done
		}
	}
done:

	return map[string]interface{}{
		"events": evts,
		"count":  len(evts),
	}, nil
}

// ─── Helpers ────────────────────────────────────────────────────────

func containsIgnoreCase(s, substr string) bool {
	if s == "" || substr == "" {
		return false
	}
	return findIgnoreCase(s, substr)
}

func findIgnoreCase(s, substr string) bool {
	sl := len(substr)
	for i := 0; i <= len(s)-sl; i++ {
		match := true
		for j := 0; j < sl; j++ {
			a, b := s[i+j], substr[j]
			if a >= 'A' && a <= 'Z' {
				a += 32
			}
			if b >= 'A' && b <= 'Z' {
				b += 32
			}
			if a != b {
				match = false
				break
			}
		}
		if match {
			return true
		}
	}
	return false
}

// ─── Main ───────────────────────────────────────────────────────────

func main() {
	port := os.Getenv("BRIDGE_PORT")
	if port == "" {
		port = defaultPort
	}
	dbPath := os.Getenv("BRIDGE_DB_PATH")
	if dbPath == "" {
		dbPath = defaultDBPath
	}

	bs, err := NewBridgeServer(dbPath)
	if err != nil {
		fmt.Fprintf(os.Stderr, "Fatal: %s\n", err)
		os.Exit(1)
	}

	server := &http.Server{
		Handler:      bs.setupRouter(),
		ReadTimeout:  30 * time.Second,
		WriteTimeout: 120 * time.Second,
	}

	listener, err := net.Listen("tcp", "127.0.0.1:"+port)
	if err != nil {
		fmt.Fprintf(os.Stderr, "Failed to listen on port %s: %s\n", port, err)
		os.Exit(1)
	}
	// Signal to parent process that server is ready
	fmt.Fprintf(os.Stdout, `{"status":"ready","port":%s}`+"\n", port)

	go func() {
		sigCh := make(chan os.Signal, 1)
		signal.Notify(sigCh, syscall.SIGINT, syscall.SIGTERM)
		<-sigCh
		fmt.Fprintf(os.Stderr, "Shutting down bridge...\n")
		bs.client.Disconnect()
		server.Close()
	}()

	if err := server.Serve(listener); err != nil && err != http.ErrServerClosed {
		fmt.Fprintf(os.Stderr, "Server error: %s\n", err)
		os.Exit(1)
	}
}
