use std::io::Result;

fn main() -> Result<()> {
    let mut config = prost_build::Config::default();
    config.compile_protos(
        &[
            "proto/waE2E/WAWebProtobufsE2E.proto",
            "proto/waCommon/WACommon.proto",
            "proto/waMultiDevice/WAMultiDevice.proto",
            "proto/waWa6/WAWebProtobufsWa6.proto",
            "proto/waAICommonDeprecated/WAAICommonDeprecated.proto",
            "proto/waCert/WACert.proto",
            "proto/signal.proto",
        ],
        &["proto/"],
    )?;
    Ok(())
}
