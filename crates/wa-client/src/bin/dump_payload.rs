use wa_client::store::DeviceStore;
use std::sync::Arc;
use tokio::sync::Mutex;
use prost::Message;

#[tokio::main]
async fn main() {
    let mut g = DeviceStore::new();
    g.registration_id = 0x12345678;
    g.signed_prekey.key_id = 0x11223344;
    
    let mut ik = [0u8; 32]; ik[0] = 5; // DJB type
    g.identity_key_pub = ik;
    
    g.signed_prekey.pub_key = [0u8; 32];
    g.signed_prekey.pub_key[0] = 5;
    g.signed_prekey.signature = vec![0u8; 64];
    
    let mut client_payload = wa_client::proto::wa_web_protobufs_wa6::ClientPayload {
        user_agent: Some(wa_client::proto::wa_web_protobufs_wa6::client_payload::UserAgent {
            platform: Some(wa_client::proto::wa_web_protobufs_wa6::client_payload::user_agent::Platform::Web.into()),
            release_channel: Some(wa_client::proto::wa_web_protobufs_wa6::client_payload::user_agent::ReleaseChannel::Release.into()),
            app_version: Some(wa_client::proto::wa_web_protobufs_wa6::client_payload::user_agent::AppVersion {
                primary: Some(2),
                secondary: Some(3000),
                tertiary: Some(1035920091),
                ..Default::default()
            }),
            mcc: Some("000".to_string()),
            mnc: Some("000".to_string()),
            os_version: Some("0.1.0".to_string()),
            manufacturer: Some(String::new()),
            device: Some("Desktop".to_string()),
            os_build_number: Some("0.1.0".to_string()),
            locale_language_iso6391: Some("en".to_string()),
            locale_country_iso31661_alpha2: Some("US".to_string()),
            ..Default::default()
        }),
        web_info: Some(wa_client::proto::wa_web_protobufs_wa6::client_payload::WebInfo {
            web_sub_platform: Some(wa_client::proto::wa_web_protobufs_wa6::client_payload::web_info::WebSubPlatform::WebBrowser.into()),
            ..Default::default()
        }),
        connect_type: Some(wa_client::proto::wa_web_protobufs_wa6::client_payload::ConnectType::WifiUnknown.into()),
        connect_reason: Some(wa_client::proto::wa_web_protobufs_wa6::client_payload::ConnectReason::UserActivated.into()),
        ..Default::default()
    };

    let mut reg_id_bytes = [0u8; 4];
    reg_id_bytes.copy_from_slice(&g.registration_id.to_be_bytes());

    let mut skey_id_bytes = [0u8; 4];
    skey_id_bytes.copy_from_slice(&g.signed_prekey.key_id.to_be_bytes());

    let device_props = wa_client::proto::wa_companion_reg::DeviceProps {
        os: Some("whatsmeow".to_string()),
        version: Some(wa_client::proto::wa_companion_reg::device_props::AppVersion {
            primary: Some(0),
            secondary: Some(1),
            tertiary: Some(0),
            ..Default::default()
        }),
        platform_type: Some(wa_client::proto::wa_companion_reg::device_props::PlatformType::Unknown.into()),
        require_full_sync: Some(false),
        history_sync_config: Some(wa_client::proto::wa_companion_reg::device_props::HistorySyncConfig {
            full_sync_days_limit: None,
            full_sync_size_mb_limit: None,
            storage_quota_mb: Some(10240),
            inline_initial_payload_in_e2_ee_msg: Some(true),
            recent_sync_days_limit: None,
            support_call_log_history: Some(false),
            support_bot_user_agent_chat_history: Some(true),
            support_cag_reactions_and_polls: Some(true),
            support_biz_hosted_msg: Some(true),
            support_recent_sync_chunk_message_count_tuning: Some(true),
            support_hosted_group_msg: Some(true),
            support_fbid_bot_chat_history: Some(true),
            support_add_on_history_sync_migration: None,
            support_message_association: Some(true),
            support_group_history: Some(true),
            on_demand_ready: None,
            support_guest_chat: None,
            complete_on_demand_ready: None,
            thumbnail_sync_days_limit: Some(60),
            initial_sync_max_messages_per_chat: None,
            support_manus_history: Some(true),
            support_hatch_history: Some(true),
        }),
        ..Default::default()
    };

    let device_props_bytes = device_props.encode_to_vec();

    client_payload.device_pairing_data = Some(wa_client::proto::wa_web_protobufs_wa6::client_payload::DevicePairingRegistrationData {
        e_regid: Some(reg_id_bytes.to_vec()),
        e_keytype: Some(vec![5]),
        e_ident: Some(g.identity_key_pub.to_vec()),
        e_skey_id: Some(skey_id_bytes[1..].to_vec()),
        e_skey_val: Some(g.signed_prekey.pub_key.to_vec()),
        e_skey_sig: Some(g.signed_prekey.signature.clone()),
        build_hash: Some(vec![211, 73, 16, 53, 118, 193, 129, 58, 170, 79, 121, 172, 64, 243, 83, 192]),
        device_props: Some(device_props_bytes),
    });
    client_payload.passive = Some(false);
    client_payload.pull = Some(false);
            
	println!("RUST_PAYLOAD_HEX:{:02x?}", hex::encode(client_payload.encode_to_vec()));
}
