//! The credential check a room configuration must pass before it is put in a
//! token.
//!
//! Ports `RoomConfiguration.CheckCredentials`. A token is handed to the client,
//! so anything inside it is public to that client. An egress output carrying an
//! S3 secret, a GCP service account, an Azure account key or a stream key
//! would therefore be published to every participant that holds the token; the
//! Go server refuses to mint such a token unless the caller opts in, and so
//! does this.

use lk_proto::livekit::{
    AliOssUpload, AutoTrackEgress, AzureBlobUpload, EncodedFileOutput, GcpUpload, ImageOutput,
    RoomConfiguration, S3Upload, SegmentedFileOutput, auto_track_egress, encoded_file_output,
    image_output, segmented_file_output,
};

use crate::error::{Error, Result};

/// Rejects a room configuration whose egress outputs carry credentials.
///
/// # Errors
///
/// Returns [`Error::SensitiveCredentials`] when any output holds a secret, or
/// when the room composite has stream outputs at all, since a stream URL is
/// itself a stream key.
pub fn check_room_configuration(config: &RoomConfiguration) -> Result<()> {
    let Some(egress) = &config.egress else {
        return Ok(());
    };

    if let Some(participant) = &egress.participant {
        check_file_outputs(&participant.file_outputs)?;
        check_segment_outputs(&participant.segment_outputs)?;
    }
    if let Some(room) = &egress.room {
        check_file_outputs(&room.file_outputs)?;
        check_segment_outputs(&room.segment_outputs)?;
        check_image_outputs(&room.image_outputs)?;
        if !room.stream_outputs.is_empty() {
            // A stream output's URL carries the stream key.
            return Err(Error::SensitiveCredentials);
        }
    }
    if let Some(tracks) = &egress.tracks {
        check_track_output(tracks)?;
    }
    Ok(())
}

fn check_file_outputs(outputs: &[EncodedFileOutput]) -> Result<()> {
    for output in outputs {
        match &output.output {
            Some(encoded_file_output::Output::S3(upload)) => check_s3(upload)?,
            Some(encoded_file_output::Output::Gcp(upload)) => check_gcp(upload)?,
            Some(encoded_file_output::Output::Azure(upload)) => check_azure(upload)?,
            Some(encoded_file_output::Output::AliOss(upload)) => check_ali_oss(upload)?,
            None => {}
        }
    }
    Ok(())
}

fn check_segment_outputs(outputs: &[SegmentedFileOutput]) -> Result<()> {
    for output in outputs {
        match &output.output {
            Some(segmented_file_output::Output::S3(upload)) => check_s3(upload)?,
            Some(segmented_file_output::Output::Gcp(upload)) => check_gcp(upload)?,
            Some(segmented_file_output::Output::Azure(upload)) => check_azure(upload)?,
            Some(segmented_file_output::Output::AliOss(upload)) => check_ali_oss(upload)?,
            None => {}
        }
    }
    Ok(())
}

fn check_image_outputs(outputs: &[ImageOutput]) -> Result<()> {
    for output in outputs {
        match &output.output {
            Some(image_output::Output::S3(upload)) => check_s3(upload)?,
            Some(image_output::Output::Gcp(upload)) => check_gcp(upload)?,
            Some(image_output::Output::Azure(upload)) => check_azure(upload)?,
            Some(image_output::Output::AliOss(upload)) => check_ali_oss(upload)?,
            None => {}
        }
    }
    Ok(())
}

fn check_track_output(tracks: &AutoTrackEgress) -> Result<()> {
    match &tracks.output {
        Some(auto_track_egress::Output::S3(upload)) => check_s3(upload),
        Some(auto_track_egress::Output::Gcp(upload)) => check_gcp(upload),
        Some(auto_track_egress::Output::Azure(upload)) => check_azure(upload),
        Some(auto_track_egress::Output::AliOss(upload)) => check_ali_oss(upload),
        None => Ok(()),
    }
}

fn check_s3(upload: &S3Upload) -> Result<()> {
    if upload.secret.is_empty() {
        Ok(())
    } else {
        Err(Error::SensitiveCredentials)
    }
}

fn check_gcp(upload: &GcpUpload) -> Result<()> {
    if upload.credentials.is_empty() {
        Ok(())
    } else {
        Err(Error::SensitiveCredentials)
    }
}

fn check_azure(upload: &AzureBlobUpload) -> Result<()> {
    if upload.account_key.is_empty() {
        Ok(())
    } else {
        Err(Error::SensitiveCredentials)
    }
}

fn check_ali_oss(upload: &AliOssUpload) -> Result<()> {
    if upload.secret.is_empty() {
        Ok(())
    } else {
        Err(Error::SensitiveCredentials)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use lk_proto::livekit::{
        AutoParticipantEgress, RoomCompositeEgressRequest, RoomEgress, StreamOutput,
    };

    use super::*;

    #[test]
    fn a_configuration_without_egress_passes() {
        let config = RoomConfiguration::default();
        assert!(check_room_configuration(&config).is_ok());
    }

    #[test]
    fn an_s3_secret_is_refused() {
        let config = RoomConfiguration {
            egress: Some(RoomEgress {
                participant: Some(AutoParticipantEgress {
                    file_outputs: vec![EncodedFileOutput {
                        output: Some(encoded_file_output::Output::S3(S3Upload {
                            secret: "shhh".to_owned(),
                            ..S3Upload::default()
                        })),
                        ..EncodedFileOutput::default()
                    }],
                    ..AutoParticipantEgress::default()
                }),
                ..RoomEgress::default()
            }),
            ..RoomConfiguration::default()
        };
        assert!(matches!(
            check_room_configuration(&config),
            Err(Error::SensitiveCredentials)
        ));
    }

    #[test]
    fn an_output_without_credentials_passes() {
        let config = RoomConfiguration {
            egress: Some(RoomEgress {
                participant: Some(AutoParticipantEgress {
                    file_outputs: vec![EncodedFileOutput {
                        filepath: "recording.mp4".to_owned(),
                        output: Some(encoded_file_output::Output::S3(S3Upload {
                            bucket: "recordings".to_owned(),
                            ..S3Upload::default()
                        })),
                        ..EncodedFileOutput::default()
                    }],
                    ..AutoParticipantEgress::default()
                }),
                ..RoomEgress::default()
            }),
            ..RoomConfiguration::default()
        };
        assert!(check_room_configuration(&config).is_ok());
    }

    #[test]
    fn any_stream_output_is_refused() {
        // the stream URL carries the stream key, so there is nothing to inspect
        let config = RoomConfiguration {
            egress: Some(RoomEgress {
                room: Some(RoomCompositeEgressRequest {
                    stream_outputs: vec![StreamOutput::default()],
                    ..RoomCompositeEgressRequest::default()
                }),
                ..RoomEgress::default()
            }),
            ..RoomConfiguration::default()
        };
        assert!(matches!(
            check_room_configuration(&config),
            Err(Error::SensitiveCredentials)
        ));
    }
}
