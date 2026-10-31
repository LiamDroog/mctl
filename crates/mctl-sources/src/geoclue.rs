//! Observer location from GeoClue2 over the system D-Bus (step 2 of the chain).
//!
//! Best effort: GeoClue only answers applications listed in `/etc/geoclue/geoclue.conf` (or approved
//! by a desktop agent), and its network backends vary by distribution. Every failure comes back with
//! the reason, so the UI can say why the fallback was used.

use std::time::Duration;

use futures_util::StreamExt;
use zbus::OwnedObjectPath;
use zbus::{Connection, Proxy};

const SERVICE: &str = "org.freedesktop.GeoClue2";
/// `GCLUE_ACCURACY_LEVEL_CITY`.
const ACCURACY_CITY: u32 = 4;
/// Identifier GeoClue matches against its configuration.
pub const DESKTOP_ID: &str = "mctl";

#[derive(Debug, Clone, PartialEq)]
pub struct Located {
    pub lat_deg: f64,
    pub lon_deg: f64,
    pub alt_m: f64,
    pub accuracy_m: f64,
    pub description: String,
}

#[derive(Debug, thiserror::Error)]
pub enum GeoclueError {
    #[error("GeoClue is not reachable on the system bus: {0}")]
    Unavailable(String),
    #[error(
        "GeoClue refused this application; add a [{DESKTOP_ID}] section with `allowed=true` and `system=false` to /etc/geoclue/geoclue.conf ({0})"
    )]
    Denied(String),
    #[error("no location within {0} s")]
    Timeout(f64),
    #[error("GeoClue: {0}")]
    Other(String),
}

fn classify(e: zbus::Error) -> GeoclueError {
    let text = e.to_string();
    if text.contains("AccessDenied")
        || text.contains("not authorized")
        || text.contains("Not authorized")
    {
        GeoclueError::Denied(text)
    } else if text.contains("ServiceUnknown")
        || text.contains("NameHasNoOwner")
        || text.contains("Failed to connect")
    {
        GeoclueError::Unavailable(text)
    } else {
        GeoclueError::Other(text)
    }
}

async fn locate_async(timeout_s: f64) -> Result<Located, GeoclueError> {
    let conn = Connection::system()
        .await
        .map_err(|e| GeoclueError::Unavailable(e.to_string()))?;
    let manager = Proxy::new(
        &conn,
        SERVICE,
        "/org/freedesktop/GeoClue2/Manager",
        "org.freedesktop.GeoClue2.Manager",
    )
    .await
    .map_err(classify)?;
    let path: OwnedObjectPath = manager.call("GetClient", &()).await.map_err(classify)?;
    let client = Proxy::new(
        &conn,
        SERVICE,
        path.as_str().to_owned(),
        "org.freedesktop.GeoClue2.Client",
    )
    .await
    .map_err(classify)?;
    client
        .set_property("DesktopId", DESKTOP_ID)
        .await
        .map_err(|e| classify(e.into()))?;
    client
        .set_property("RequestedAccuracyLevel", ACCURACY_CITY)
        .await
        .map_err(|e| classify(e.into()))?;
    let mut updates = client
        .receive_signal("LocationUpdated")
        .await
        .map_err(classify)?;
    client.call_method("Start", &()).await.map_err(classify)?;
    let result = async {
        let loc_path =
            match tokio::time::timeout(Duration::from_secs_f64(timeout_s), updates.next()).await {
                Err(_) => return Err(GeoclueError::Timeout(timeout_s)),
                Ok(None) => return Err(GeoclueError::Other("signal stream closed".into())),
                Ok(Some(msg)) => {
                    let (_old, new): (OwnedObjectPath, OwnedObjectPath) = msg
                        .body()
                        .deserialize()
                        .map_err(|e| GeoclueError::Other(e.to_string()))?;
                    new
                }
            };
        let loc = Proxy::new(
            &conn,
            SERVICE,
            loc_path.as_str().to_owned(),
            "org.freedesktop.GeoClue2.Location",
        )
        .await
        .map_err(classify)?;
        let get = |name: &'static str| {
            let loc = &loc;
            async move {
                loc.get_property::<f64>(name)
                    .await
                    .map_err(|e| GeoclueError::Other(format!("{name}: {e}")))
            }
        };
        Ok(Located {
            lat_deg: get("Latitude").await?,
            lon_deg: get("Longitude").await?,
            // GeoClue reports -1.79769e+308 when it has no altitude.
            alt_m: get("Altitude")
                .await
                .ok()
                .filter(|a| a.is_finite() && *a > -1.0e6)
                .unwrap_or(0.0),
            accuracy_m: get("Accuracy").await?,
            description: loc
                .get_property::<String>("Description")
                .await
                .unwrap_or_default(),
        })
    }
    .await;
    let _ = client.call_method("Stop", &()).await;
    result
}

/// Ask GeoClue for a city-level location; blocks for up to `timeout_s`.
pub fn locate(timeout_s: f64) -> Result<Located, GeoclueError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| GeoclueError::Other(e.to_string()))?;
    rt.block_on(locate_async(timeout_s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_classified_so_the_ui_can_say_what_to_do() {
        let denied = classify(zbus::Error::Failure(
            "org.freedesktop.DBus.Error.AccessDenied: not authorized".into(),
        ));
        assert!(matches!(denied, GeoclueError::Denied(_)));
        assert!(denied.to_string().contains("/etc/geoclue/geoclue.conf"));
        assert!(matches!(
            classify(zbus::Error::Failure("ServiceUnknown".into())),
            GeoclueError::Unavailable(_)
        ));
        assert!(matches!(
            classify(zbus::Error::Failure("something else".into())),
            GeoclueError::Other(_)
        ));
    }

    /// Needs a running GeoClue; with this application not whitelisted it must report `Denied`, not hang.
    #[test]
    #[ignore = "needs a session with GeoClue running"]
    fn locating_returns_a_location_or_a_clear_reason() {
        match locate(3.0) {
            Ok(l) => {
                println!("GeoClue located: {l:?}");
                assert!(l.lat_deg.abs() <= 90.0 && l.lon_deg.abs() <= 180.0, "{l:?}");
            }
            Err(e) => println!("GeoClue said: {e}"),
        }
    }
}
