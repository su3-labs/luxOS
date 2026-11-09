//! Local NetworkManager setup portal. Wi-Fi credentials go directly to root-owned keyfiles.
use anyhow::{Context, Result, ensure};
use axum::{
    Form, Router,
    extract::{ConnectInfo, DefaultBodyLimit, State},
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::post,
};
use serde::Deserialize;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::SocketAddr,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::{net::TcpListener, process::Command, sync::Mutex, task::JoinHandle, time};
use uuid::Uuid;

const AP: &str = "SU3-Raman-Setup";
const ADDRESS: &str = "10.42.77.1";
const PROFILES: &str = "/etc/NetworkManager/system-connections";
const STYLE: &str = "<style>html{color-scheme:dark}body{background:#101318;color:#edf1f3;font:16px system-ui;max-width:34rem;margin:10vh auto;padding:0 1.3rem}.mark{color:#62e5bc;font:700 1rem ui-monospace,monospace}h1{font-size:2.5rem;letter-spacing:-.05em}p,small{color:#b9c3cb}label{display:block;margin:1rem 0}input:not([type=checkbox]),button{display:block;width:100%;box-sizing:border-box;font:inherit;padding:.65rem;margin-top:.35rem}button{color:#101318;background:#62e5bc;font-weight:700}a{color:#62e5bc}</style>";

pub fn passphrase() -> Result<String> {
    let mut bytes = [0_u8; 24];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

async fn nm(args: &[&str], seconds: u64) -> Result<String> {
    let output = time::timeout(
        Duration::from_secs(seconds),
        Command::new("nmcli").args(args).kill_on_drop(true).output(),
    )
    .await
    .context("NetworkManager timed out")??;
    ensure!(output.status.success(), "NetworkManager command failed");
    Ok(String::from_utf8(output.stdout)?)
}

async fn wifi_interface() -> Result<Option<String>> {
    Ok(nm(&["-t", "-f", "DEVICE,TYPE", "device", "status"], 10)
        .await?
        .lines()
        .find_map(|line| line.strip_suffix(":wifi").map(str::to_owned)))
}

async fn active(iface: &str) -> Result<String> {
    Ok(
        nm(&["-g", "GENERAL.CONNECTION", "device", "show", iface], 10)
            .await?
            .trim()
            .into(),
    )
}

fn write_profile(path: &Path, contents: &str) -> Result<()> {
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?
        .write_all(contents.as_bytes())?;
    Ok(())
}

async fn start_ap(iface: &str) -> Result<()> {
    if active(iface).await? == AP {
        return Ok(());
    }
    let path = Path::new(PROFILES).join("su3-setup.nmconnection");
    if !path.exists() {
        let password = fs::read_to_string("/etc/su3/setup-passphrase")?;
        let password = password.trim_end();
        ensure!(
            (8..=63).contains(&password.len()) && password.bytes().all(|c| (32..=126).contains(&c)),
            "invalid setup passphrase"
        );
        write_profile(
            &path,
            &format!(
                "[connection]\nid={AP}\nuuid={}\ntype=wifi\nautoconnect=false\n[wifi]\nssid={AP}\nmode=ap\nband=bg\npowersave=2\n[wifi-security]\nkey-mgmt=wpa-psk\npsk={}\n[ipv4]\nmethod=shared\naddress1={ADDRESS}/24\n[ipv6]\nmethod=disabled\n",
                Uuid::new_v4(),
                escaped(password)
            ),
        )?;
    }
    nm(&["connection", "load", path.to_str().unwrap()], 20).await?;
    nm(
        &["--wait", "30", "connection", "up", AP, "ifname", iface],
        36,
    )
    .await?;
    Ok(())
}

fn escaped(value: &str) -> String {
    value.replace('\\', "\\\\").replace(' ', "\\s")
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct SetupForm {
    csrf: String,
    ssid: String,
    password: String,
    username: String,
    domain: String,
    open: String,
}

fn security(form: &SetupForm) -> Result<String, &'static str> {
    if !(1..=32).contains(&form.ssid.len()) || form.ssid.chars().any(char::is_control) {
        return Err("Enter a Wi-Fi name of 1–32 bytes.");
    }
    if form
        .password
        .chars()
        .chain(form.username.chars())
        .any(char::is_control)
    {
        return Err("Remove control characters from the credentials.");
    }
    let domain = form.domain.trim();
    if form.open == "1" {
        if !form.password.is_empty() || !form.username.is_empty() || !domain.is_empty() {
            return Err("Leave credentials blank for an open network.");
        }
        return Ok(String::new());
    }
    if !form.username.is_empty() {
        let valid_domain = domain.len() <= 253
            && domain.split('.').all(|label| {
                !label.is_empty()
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            });
        if form.password.is_empty() || !valid_domain {
            return Err("Enterprise Wi-Fi requires a password and the RADIUS server domain.");
        }
        return Ok(format!(
            "[wifi-security]\nkey-mgmt=wpa-eap\n[802-1x]\neap=peap;\nidentity={}\npassword={}\nphase2-auth=mschapv2\nsystem-ca-certs=true\ndomain-suffix-match={domain}\n",
            escaped(&form.username),
            escaped(&form.password)
        ));
    }
    if !(8..=63).contains(&form.password.len())
        && !(form.password.len() == 64 && form.password.bytes().all(|c| c.is_ascii_hexdigit()))
    {
        return Err("Enter an 8–63 byte Wi-Fi password.");
    }
    Ok(format!(
        "[wifi-security]\nkey-mgmt=wpa-psk\npsk={}\n",
        escaped(&form.password)
    ))
}

async fn join(iface: &str, ssid: &str, security: &str) -> Result<()> {
    let key = Uuid::new_v4().to_string();
    let path = Path::new(PROFILES).join(format!("su3-wifi-{key}.nmconnection"));
    let ssid: String = ssid.bytes().map(|b| format!("{b};")).collect();
    write_profile(
        &path,
        &format!(
            "[connection]\nid=SU3 Wi-Fi {}\nuuid={key}\ntype=wifi\nautoconnect=false\n[wifi]\nssid={ssid}\nmode=infrastructure\ncloned-mac-address=permanent\n{security}[ipv4]\nmethod=auto\n[ipv6]\nmethod=auto\n",
            &key[..8]
        ),
    )?;
    let activation = async {
        nm(&["connection", "load", path.to_str().unwrap()], 20).await?;
        nm(
            &[
                "--wait",
                "40",
                "connection",
                "up",
                "uuid",
                &key,
                "ifname",
                iface,
            ],
            46,
        )
        .await?;
        nm(
            &[
                "connection",
                "modify",
                "uuid",
                &key,
                "connection.autoconnect",
                "yes",
                "connection.autoconnect-priority",
                "100",
            ],
            20,
        )
        .await?;
        Ok(())
    }
    .await;
    if activation.is_err() {
        let _ = nm(&["connection", "delete", "uuid", &key], 20).await;
        let _ = fs::remove_file(&path);
        let _ = start_ap(iface).await;
        return activation;
    }
    for old in fs::read_dir(PROFILES)?.flatten() {
        let name = old.file_name();
        if let Some(uuid) = name
            .to_str()
            .and_then(|name| name.strip_prefix("su3-wifi-"))
            .and_then(|name| name.strip_suffix(".nmconnection"))
            && uuid != key
            && Uuid::parse_str(uuid).is_ok()
        {
            let _ = nm(&["connection", "delete", "uuid", uuid], 20).await;
        }
    }
    Ok(())
}

#[derive(Clone)]
struct Portal {
    iface: String,
    site: String,
    token: String,
    busy: Arc<Mutex<()>>,
    failed: Arc<RwLock<bool>>,
}

impl Portal {
    fn page(&self, status: StatusCode, title: &str, body: String) -> Response {
        let html = format!(
            r#"<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>{title} · SU(3)</title>{STYLE}<main><div class="mark">SU(3) LABS / RAMAN</div><h1>{title}</h1>{body}</main></html>"#
        );
        let mut response = (status, Html(html)).into_response();
        let headers = response.headers_mut();
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'"
        ));
        headers.insert(
            header::SET_COOKIE,
            HeaderValue::from_str(&format!(
                "su3_setup={}; HttpOnly; SameSite=Strict; Path=/",
                self.token
            ))
            .unwrap(),
        );
        response
    }
}

fn from_setup(address: SocketAddr) -> bool {
    matches!(address.ip(), std::net::IpAddr::V4(ip) if ip.octets()[..3] == [10, 42, 77])
}

async fn setup_page(
    State(portal): State<Portal>,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if !from_setup(address) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if headers.get(header::HOST).is_none_or(|v| v != ADDRESS) || uri.path() != "/" {
        return Redirect::to(&format!("http://{ADDRESS}/")).into_response();
    }
    let message = if *portal.failed.read().unwrap() {
        "<p>Could not join that network. Check the details and try again.</p>"
    } else {
        ""
    };
    portal.page(StatusCode::OK, "Connect to Wi-Fi", format!(r#"{message}<p>Join this Raman node to your network.</p>
<form action="/connect" method="post">
<input type="hidden" name="csrf" value="{}">
<label>Wi-Fi network name <input name="ssid" maxlength="32" required></label>
<label>Password <input name="password" type="password" maxlength="64" autocomplete="new-password"></label>
<details><summary>Enterprise Wi-Fi</summary>
<label>Username <input name="username" autocomplete="username"></label>
<label>RADIUS server domain <input name="domain" placeholder="radius.example.org"></label>
<small>PEAP with MSCHAPv2 and a system-trusted certificate.</small></details>
<label><input type="checkbox" name="open" value="1"> Open network</label>
<button type="submit">Connect</button></form>"#, portal.token))
}

async fn connect(
    State(portal): State<Portal>,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<SetupForm>,
) -> Response {
    let cookie = format!("su3_setup={}", portal.token);
    if !from_setup(address)
        || headers.get(header::HOST).is_none_or(|v| v != ADDRESS)
        || headers
            .get(header::ORIGIN)
            .is_some_and(|v| v != "http://10.42.77.1")
        || form.csrf != portal.token
        || !headers
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.split(';').any(|c| c.trim() == cookie))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let security = match security(&form) {
        Ok(value) => value,
        Err(message) => {
            return portal.page(
                StatusCode::BAD_REQUEST,
                "Check Wi-Fi details",
                format!("<p>{message}</p><a href='/'>Back</a>"),
            );
        }
    };
    let Ok(permit) = portal.busy.clone().try_lock_owned() else {
        return portal.page(
            StatusCode::CONFLICT,
            "Connecting",
            "<p>A Wi-Fi connection attempt is in progress.</p>".into(),
        );
    };
    let response = portal.page(
        StatusCode::OK,
        "Connecting",
        format!(
            "<p>Reconnect your phone to your Wi-Fi, then open <a href='{}'>{}</a>.</p>",
            portal.site, portal.site
        ),
    );
    tokio::spawn(async move {
        let _permit = permit;
        time::sleep(Duration::from_secs(1)).await;
        *portal.failed.write().unwrap() = join(&portal.iface, &form.ssid, &security).await.is_err();
    });
    response
}

async fn watch(
    portal: &mut Option<JoinHandle<()>>,
    disconnected: &mut u8,
    busy: &Arc<Mutex<()>>,
    failed: &Arc<RwLock<bool>>,
    site: &str,
) -> Result<()> {
    let Ok(_permit) = busy.try_lock() else {
        return Ok(());
    };
    let Some(iface) = wifi_interface().await? else {
        return Ok(());
    };
    let mut active = active(&iface).await?;
    if active.is_empty() || active == "--" {
        *disconnected = disconnected.saturating_add(1);
        if *disconnected >= 4 {
            start_ap(&iface).await?;
            active = AP.into();
        }
    } else {
        *disconnected = 0;
    }
    if portal
        .as_ref()
        .is_some_and(|server| server.is_finished() || active != AP)
        && let Some(server) = portal.take()
    {
        server.abort();
    }
    if active == AP && portal.is_none() {
        let listener = TcpListener::bind((ADDRESS, 80)).await?;
        let app = Router::new()
            .route("/connect", post(connect))
            .fallback(setup_page)
            .layer(DefaultBodyLimit::max(4096))
            .with_state(Portal {
                iface,
                token: passphrase()?,
                site: site.into(),
                busy: busy.clone(),
                failed: failed.clone(),
            });
        *portal = Some(tokio::spawn(async move {
            if let Err(error) = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            {
                eprintln!("Wi-Fi portal stopped: {error}");
            }
        }));
        eprintln!("Wi-Fi setup hotspot active");
    }
    Ok(())
}

pub async fn run() -> Result<()> {
    let name = hostname::get()?;
    let name = name
        .to_str()
        .and_then(|v| v.split('.').next())
        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        .unwrap_or("su3-raman");
    let site = format!("http://{name}.local:8000/");
    let busy = Arc::new(Mutex::new(()));
    let failed = Arc::new(RwLock::new(false));
    let mut server = None;
    let mut disconnected = 0;
    let _ = nm(&["radio", "wifi", "on"], 10).await;
    loop {
        if let Err(error) = watch(&mut server, &mut disconnected, &busy, &failed, &site).await {
            eprintln!("Wi-Fi setup: {error:#}");
        }
        time::sleep(Duration::from_secs(3)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_personal_and_enterprise_modes() {
        let mut form = SetupForm {
            ssid: "Lab Wi-Fi".into(),
            password: "pass word-123".into(),
            ..Default::default()
        };
        assert!(security(&form).unwrap().contains("psk=pass\\sword-123"));
        form.username = "tester".into();
        assert!(security(&form).is_err());
        form.domain = "radius.example.org".into();
        assert!(security(&form).unwrap().contains("system-ca-certs=true"));
    }
}
