pub mod binary;
pub mod socket;
pub mod client;
pub mod store;
pub mod crypto;
pub mod usync;
pub mod qr;

pub mod proto {
    pub mod wa_web_protobufs_e2e {
        include!(concat!(env!("OUT_DIR"), "/wa_web_protobufs_e2e.rs"));
    }
    pub mod wa_common {
        include!(concat!(env!("OUT_DIR"), "/wa_common.rs"));
    }
    pub mod wa_multi_device {
        include!(concat!(env!("OUT_DIR"), "/wa_multi_device.rs"));
    }
    pub mod wa_web_protobufs_wa6 {
        include!(concat!(env!("OUT_DIR"), "/wa_web_protobufs_wa6.rs"));
    }
    pub mod wa_cert {
        include!(concat!(env!("OUT_DIR"), "/wa_cert.rs"));
    }
    pub mod wa_web_protobufs_ai_common {
        include!(concat!(env!("OUT_DIR"), "/wa_web_protobufs_ai_common.rs"));
    }
    pub mod waai_common_deprecated {
        include!(concat!(env!("OUT_DIR"), "/waai_common_deprecated.rs"));
    }
    pub mod wa_companion_reg {
        include!(concat!(env!("OUT_DIR"), "/wa_companion_reg.rs"));
    }
    pub mod wa_status_attributions {
        include!(concat!(env!("OUT_DIR"), "/wa_status_attributions.rs"));
    }
    pub mod wa_adv {
        include!(concat!(env!("OUT_DIR"), "/wa_adv.rs"));
    }
    pub mod wa_mms_retry {
        include!(concat!(env!("OUT_DIR"), "/wa_mms_retry.rs"));
    }
    pub mod signal {
        include!(concat!(env!("OUT_DIR"), "/signal.rs"));
    }
}
