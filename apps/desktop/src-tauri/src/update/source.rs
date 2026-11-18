//! Updater check and download interface.

use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use strata_types::Code;
use tauri::{AppHandle, Runtime, Url};
use tauri_plugin_updater::{Update, Updater, UpdaterExt};

use super::UpdateFuture;
use crate::error::GuiError;

use super::manifest::{self, Manifest, PLATFORM};

/// Metadata returned by the update check. Manifest parsing uses `raw_json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateCandidate {
    pub version: String,
    pub notes: Option<String>,
    pub raw_json: Value,
}

/// A verified archive and the install operation from the same checked update.
pub struct VerifiedUpdate {
    pub bytes: Vec<u8>,
    /// Captures the updater that verified these bytes, including its signature and install destination.
    pub installer: Box<dyn FnOnce(&[u8]) -> tauri_plugin_updater::Result<()> + Send>,
}

pub trait UpdateSource: Send + Sync {
    fn check(&self) -> UpdateFuture<'_, Result<Option<UpdateCandidate>, GuiError>>;

    /// Download and verify the checked target, reporting chunk bytes and the optional total.
    fn download<'a>(
        &'a self,
        target: &'a str,
        on_progress: &'a mut (dyn FnMut(usize, Option<u64>) + Send),
    ) -> UpdateFuture<'a, Result<VerifiedUpdate, GuiError>>;
}

pub(crate) struct PluginUpdateSource {
    updater: Updater,
    endpoint: Url,
    installed: String,
    checked: Mutex<Option<Update>>,
}

impl PluginUpdateSource {
    pub(crate) fn new<R: Runtime>(app: &AppHandle<R>, installed: String) -> Result<Self, GuiError> {
        let endpoint = app
            .config()
            .plugins
            .0
            .get("updater")
            .and_then(|config| config.get("endpoints"))
            .and_then(|endpoints| endpoints.as_array())
            .and_then(|endpoints| match endpoints.as_slice() {
                [endpoint] => endpoint.as_str(),
                _ => None,
            })
            .and_then(|endpoint| Url::parse(endpoint).ok())
            .filter(|endpoint| {
                endpoint.scheme() == "https" && endpoint.host_str() == Some("github.com")
            })
            .ok_or_else(|| {
                source_error("update_endpoint", "Missing GitHub HTTPS update endpoint")
            })?;
        let updater = app
            .updater_builder()
            .target(PLATFORM)
            .version_comparator(|current, release| {
                release.version.pre.is_empty() && release.version.cmp_precedence(&current).is_gt()
            })
            // Bound stalls without cutting off a download that is still making progress.
            .configure_client(|client| client.read_timeout(Duration::from_secs(300)))
            .build()
            .map_err(|_| {
                source_error(
                    "update_source_unavailable",
                    "Could not initialize the updater",
                )
            })?;
        Ok(Self {
            updater,
            endpoint,
            installed,
            checked: Mutex::new(None),
        })
    }

    pub(crate) fn restore(&self, raw_json: Value) -> Result<UpdateCandidate, GuiError> {
        let release = manifest::static_release(&raw_json)?;
        let mut candidate = UpdateCandidate {
            version: release.version.to_string(),
            notes: release.notes,
            raw_json,
        };
        let update = match manifest::read(&candidate, &self.installed)? {
            Manifest::Available(_) => Some(
                self.updater
                    .restore_update(candidate.raw_json.clone())
                    .map_err(|_| manifest::malformed_manifest())?
                    .ok_or_else(manifest::malformed_manifest)?,
            ),
            Manifest::UpToDate => None,
            // A newer release for another platform leaves the last checked handle valid.
            Manifest::NoPlatform => return Ok(candidate),
        };
        if let Some(checked) = &update {
            candidate.raw_json = checked.raw_json.clone();
        }
        *self
            .checked
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = update;
        Ok(candidate)
    }
}

impl UpdateSource for PluginUpdateSource {
    fn check(&self) -> UpdateFuture<'_, Result<Option<UpdateCandidate>, GuiError>> {
        Box::pin(async move {
            match fetch_manifest(&self.endpoint).await? {
                Some(raw) => self.restore(raw).map(Some),
                None => {
                    *self
                        .checked
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner()) = None;
                    Ok(None)
                }
            }
        })
    }

    fn download<'a>(
        &'a self,
        target: &'a str,
        on_progress: &'a mut (dyn FnMut(usize, Option<u64>) + Send),
    ) -> UpdateFuture<'a, Result<VerifiedUpdate, GuiError>> {
        Box::pin(async move {
            let update = self
                .checked
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .as_ref()
                .filter(|update| update.version == target)
                .cloned()
                .ok_or_else(|| source_error("update_target_changed", "Check for updates again"))?;
            let candidate = UpdateCandidate {
                version: update.version.clone(),
                notes: update.body.clone(),
                raw_json: update.raw_json.clone(),
            };
            require_in_app_target(&candidate, &self.installed)?;
            let bytes = update
                .download(on_progress, || {})
                .await
                .map_err(download_error)?;
            Ok(VerifiedUpdate {
                bytes,
                installer: Box::new(move |bytes| update.install(bytes)),
            })
        })
    }
}

fn require_in_app_target(candidate: &UpdateCandidate, installed: &str) -> Result<(), GuiError> {
    let Manifest::Available(release) = manifest::read(candidate, installed)? else {
        return Err(source_error(
            "update_target_changed",
            "Check for updates again",
        ));
    };
    if let Some(url) = release.manual_download_url {
        return Err(GuiError::new(
            Code::Unsupported,
            "Update by hand",
            json!({ "reason": "below_min_updatable_from", "url": url }),
        ));
    }
    Ok(())
}

fn source_error(reason: &str, message: &str) -> GuiError {
    GuiError::new(Code::Unknown, message, json!({ "reason": reason }))
}

pub(crate) fn download_error(error: tauri_plugin_updater::Error) -> GuiError {
    use tauri_plugin_updater::Error;

    match error {
        Error::Minisign(_)
        | Error::Base64(_)
        | Error::SignatureUtf8(_)
        | Error::SignedVersionMismatch { .. }
        | Error::MissingSignedVersion => source_error(
            "invalid_update_signature",
            "Could not verify the update signature",
        ),
        _ => source_error("update_download_failed", "Could not download the update"),
    }
}

fn decode_response(status: isize, body: &[u8]) -> Result<Option<Value>, GuiError> {
    match status {
        204 => Ok(None),
        200 => serde_json::from_slice(body)
            .map(Some)
            .map_err(|_| manifest::malformed_manifest()),
        403 | 429 => Err(GuiError::new(
            Code::Busy,
            "The update endpoint is rate limited",
            json!({ "reason": "update_rate_limited", "status": status }),
        )),
        _ => Err(GuiError::new(
            Code::Unknown,
            "The update endpoint returned an error",
            json!({ "reason": "update_http_status", "status": status }),
        )),
    }
}

#[cfg(target_os = "macos")]
async fn fetch_manifest(endpoint: &Url) -> Result<Option<Value>, GuiError> {
    use block2::RcBlock;
    use objc2::rc::{Retained, autoreleasepool};
    use objc2::runtime::{AnyObject, Bool};
    use objc2::{class, msg_send};
    use objc2_foundation::{NSData, NSString};
    use tokio::sync::oneshot;

    // Updater::check drops non-success HTTP statuses. Make one GET with
    // Foundation, then let the plugin restore the Update from its response.
    fn request_error() -> GuiError {
        source_error(
            "update_request_failed",
            "Could not fetch the update manifest",
        )
    }

    let (send, recv) = oneshot::channel();
    let send = Mutex::new(Some(send));
    autoreleasepool(|_| -> Result<(), GuiError> {
        let url_string = NSString::from_str(endpoint.as_str());
        let callback = RcBlock::new(
            move |data: *mut NSData, response: *mut AnyObject, error: *mut AnyObject| {
                // SAFETY: NSURLSession owns these callback arguments until we return.
                // HTTPURLResponse is the class that responds to statusCode.
                let result = unsafe {
                    let response_is_http: Bool =
                        msg_send![response, isKindOfClass: class!(NSHTTPURLResponse)];
                    if !error.is_null() || !response_is_http.as_bool() {
                        Err(request_error())
                    } else {
                        let status: isize = msg_send![response, statusCode];
                        let url: *mut AnyObject = msg_send![response, URL];
                        let scheme: Option<Retained<NSString>> = msg_send![url, scheme];
                        if scheme.is_none_or(|scheme| scheme.to_string() != "https") {
                            Err(request_error())
                        } else {
                            match (status, data.as_ref()) {
                                (200, Some(data)) if data.len() <= 1024 * 1024 => {
                                    decode_response(status, &data.to_vec())
                                }
                                (200, _) => Err(manifest::malformed_manifest()),
                                _ => decode_response(status, &[]),
                            }
                        }
                    }
                };
                if let Some(send) = send
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .take()
                {
                    let _ = send.send(result);
                }
            },
        );
        // SAFETY: the objects are constructed by Foundation on this thread.
        // The data task copies the completion block and retains the request.
        unsafe {
            let url: Option<Retained<AnyObject>> =
                msg_send![class!(NSURL), URLWithString: &*url_string];
            let url = url.ok_or_else(request_error)?;
            let request: Retained<AnyObject> =
                msg_send![class!(NSMutableURLRequest), requestWithURL: &*url];
            let _: () = msg_send![&*request, setValue: &*NSString::from_str("application/json"), forHTTPHeaderField: &*NSString::from_str("Accept")];
            let _: () = msg_send![&*request, setValue: &*NSString::from_str("git-strata-updater"), forHTTPHeaderField: &*NSString::from_str("User-Agent")];
            let config: Retained<AnyObject> = msg_send![
                class!(NSURLSessionConfiguration),
                ephemeralSessionConfiguration
            ];
            let _: () = msg_send![&*config, setTimeoutIntervalForRequest: 15.0_f64];
            let _: () = msg_send![&*config, setTimeoutIntervalForResource: 30.0_f64];
            let _: () = msg_send![&*config, setHTTPShouldSetCookies: Bool::NO];
            let _: () = msg_send![&*config, setURLCredentialStorage: std::ptr::null::<AnyObject>()];
            let session: Retained<AnyObject> =
                msg_send![class!(NSURLSession), sessionWithConfiguration: &*config];
            let task: Retained<AnyObject> =
                msg_send![&*session, dataTaskWithRequest: &*request, completionHandler: &*callback];
            let _: () = msg_send![&*task, resume];
            let _: () = msg_send![&*session, finishTasksAndInvalidate];
        }
        Ok(())
    })?;
    recv.await.map_err(|_| request_error())?
}

#[cfg(not(target_os = "macos"))]
async fn fetch_manifest(_: &Url) -> Result<Option<Value>, GuiError> {
    Err(source_error("update_platform", "No update for this Mac"))
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Default)]
    pub(crate) struct FakeUpdateSource {
        pub responses: Mutex<VecDeque<Result<Option<UpdateCandidate>, GuiError>>>,
        pub reject_signature: bool,
        pub pause_check: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }

    impl UpdateSource for FakeUpdateSource {
        fn check(&self) -> UpdateFuture<'_, Result<Option<UpdateCandidate>, GuiError>> {
            Box::pin(async {
                let pause_check = self.pause_check.lock().unwrap().take();
                if let Some(pause_check) = pause_check {
                    pause_check.await.unwrap();
                }
                self.responses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("unexpected check")
            })
        }

        fn download<'a>(
            &'a self,
            _: &'a str,
            on_progress: &'a mut (dyn FnMut(usize, Option<u64>) + Send),
        ) -> UpdateFuture<'a, Result<VerifiedUpdate, GuiError>> {
            Box::pin(async move {
                let bytes = b"fake app archive".to_vec();
                on_progress(bytes.len(), Some(bytes.len() as u64));
                if self.reject_signature {
                    return Err(download_error(tauri_plugin_updater::Error::SignatureUtf8(
                        "invalid fixture".into(),
                    )));
                }
                Ok(VerifiedUpdate {
                    bytes,
                    installer: Box::new(|_| Ok(())),
                })
            })
        }
    }

    #[test]
    fn download_guard_refuses_installed_versions_below_the_floor() {
        let candidate = UpdateCandidate {
            version: "0.4.0".into(),
            notes: None,
            raw_json: json!({
                "version": "0.4.0",
                "platforms": {
                    "darwin-aarch64": { "signature": "fixture", "url": "https://example.com/archive" }
                },
                "git_strata": { "min_updatable_from": "0.3.1" }
            }),
        };
        let below = require_in_app_target(&candidate, "0.3.0").unwrap_err();
        assert_eq!(below.reason(), Some("below_min_updatable_from"));
        assert_eq!(below.detail["url"], manifest::dmg_url("0.4.0"));
        assert!(require_in_app_target(&candidate, "0.3.1").is_ok());
    }

    #[test]
    fn http_status_is_distinct_from_parse_and_transport_errors() {
        for status in [403, 429] {
            let error = decode_response(status, &[]).unwrap_err();
            assert_eq!(error.reason(), Some("update_rate_limited"));
            assert_eq!(error.detail["status"], status);
        }
        assert_eq!(
            decode_response(503, &[]).unwrap_err().reason(),
            Some("update_http_status")
        );
        assert_eq!(
            decode_response(200, b"<html>").unwrap_err().reason(),
            Some("malformed_update_manifest")
        );
        assert_eq!(decode_response(204, &[]).unwrap(), None);
    }
}
