#!/usr/bin/env python3
"""NetworkManager onboarding for the SU(3) Raman node."""
import html
import os
import re
import secrets
import socket
import subprocess
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

ADDRESS = "10.42.77.1"
SETUP = "SU3-Raman-Setup"
HOSTNAME = socket.gethostname().split(".", 1)[0]
SITE = f"http://{HOSTNAME}.local:8000/"
PROFILE_DIR = Path("/etc/NetworkManager/system-connections")
TOKEN = secrets.token_urlsafe(24)
BUSY = threading.Lock()
MESSAGE = ""
STYLE = """<style>html{color-scheme:dark}body{background:#101318;color:#edf1f3;
font:16px system-ui;max-width:34rem;margin:10vh auto;padding:0 1.3rem}
.mark{color:#62e5bc;font:700 1rem ui-monospace,monospace;letter-spacing:.12em}
h1{font-size:2.5rem;letter-spacing:-.05em;margin:.6rem 0}p,small{color:#b9c3cb}
label{display:block;margin:1rem 0}input:not([type=checkbox]),button{display:block;
width:100%;box-sizing:border-box;font:inherit;margin-top:.35rem;padding:.65rem;
border:1px solid #45545d;border-radius:.45rem}button{color:#101318;
background:#62e5bc;font-weight:700;cursor:pointer}a{color:#62e5bc}</style>"""


def nm(*args, check=True, timeout=20):
    return subprocess.run(["nmcli", *args], check=check, capture_output=True,
                          text=True, timeout=timeout)


def wifi_device():
    rows = nm("-t", "-f", "DEVICE,TYPE", "device", "status").stdout.splitlines()
    return next((row.rsplit(":", 1)[0] for row in rows if row.endswith(":wifi")), None)


def active_profile(iface):
    return nm("-g", "GENERAL.CONNECTION", "device", "show", iface).stdout.strip()


def start_ap(iface):
    if active_profile(iface) == SETUP:
        return
    path = PROFILE_DIR / "su3-setup.nmconnection"
    if not path.exists():
        password = Path("/etc/su3/setup-passphrase").read_text().strip()
        write_profile(path, f"""[connection]
id={SETUP}
uuid={uuid.uuid4()}
type=wifi
autoconnect=false
[wifi]
ssid={SETUP}
mode=ap
band=bg
powersave=2
[wifi-security]
key-mgmt=wpa-psk
psk={password}
[ipv4]
method=shared
address1={ADDRESS}/24
[ipv6]
method=disabled
""")
    nm("connection", "load", str(path))
    nm("--wait", "30", "connection", "up", SETUP, "ifname", iface, timeout=36)


def write_profile(path, contents):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as out:
        out.write(contents)


def escaped(value):
    return value.replace("\\", "\\\\").replace(" ", "\\s")


def credentials(form):
    ssid = form.get("ssid", "")
    password = form.get("password", "")
    username = form.get("username", "")
    domain = form.get("domain", "").strip()
    open_network = form.get("open") == "1"
    if not 1 <= len(ssid.encode()) <= 32 or any(ord(c) < 32 for c in ssid):
        raise ValueError("Enter a Wi-Fi name of 1–32 bytes.")
    if any(ord(c) < 32 for c in password + username):
        raise ValueError("Remove control characters from the credentials.")
    if open_network:
        if password or username or domain:
            raise ValueError("Leave credentials blank for an open network.")
        security = ""
    elif username:
        if not password or not re.fullmatch(r"[a-zA-Z0-9](?:[a-zA-Z0-9.-]*[a-zA-Z0-9])?", domain):
            raise ValueError("Enterprise Wi-Fi requires a password and the RADIUS server domain.")
        security = f"""[wifi-security]
key-mgmt=wpa-eap
[802-1x]
eap=peap;
identity={escaped(username)}
password={escaped(password)}
phase2-auth=mschapv2
system-ca-certs=true
domain-suffix-match={domain}
"""
    else:
        raw_psk = re.fullmatch(r"[0-9a-fA-F]{64}", password)
        if not raw_psk and not 8 <= len(password.encode()) <= 63:
            raise ValueError("Enter an 8–63 byte Wi-Fi password.")
        security = f"[wifi-security]\nkey-mgmt=wpa-psk\npsk={escaped(password)}\n"
    return ssid, security


def join(iface, ssid, security):
    global MESSAGE
    key = str(uuid.uuid4())
    path = PROFILE_DIR / f"su3-wifi-{key}.nmconnection"
    ssid_bytes = "".join(f"{b};" for b in ssid.encode())
    try:
        time.sleep(1)
        write_profile(path, f"""[connection]
id=SU3 Wi-Fi {key[:8]}
uuid={key}
type=wifi
autoconnect=false
[wifi]
ssid={ssid_bytes}
mode=infrastructure
cloned-mac-address=permanent
{security}[ipv4]
method=auto
[ipv6]
method=auto
""")
        nm("connection", "load", str(path))
        nm("--wait", "40", "connection", "up", "uuid", key, "ifname", iface, timeout=46)
        nm("connection", "modify", "uuid", key, "connection.autoconnect", "yes",
           "connection.autoconnect-priority", "100")
        for old in PROFILE_DIR.glob("su3-wifi-*.nmconnection"):
            if old != path:
                nm("connection", "delete", "uuid", old.stem.removeprefix("su3-wifi-"), check=False)
        MESSAGE = ""
    except (OSError, subprocess.SubprocessError):
        MESSAGE = "Could not join that network. Check the details and try again."
        try:
            nm("connection", "delete", "uuid", key, check=False)
            path.unlink(missing_ok=True)
            start_ap(iface)
        except (OSError, subprocess.SubprocessError):
            pass
    finally:
        BUSY.release()


class Handler(BaseHTTPRequestHandler):
    server_version = "SU3-WiFi/1"

    def setup(self):
        super().setup()
        self.connection.settimeout(5)

    def log_message(self, *_):
        pass

    def page(self, title, body, status=200):
        payload = (f'<!doctype html><html lang="en"><meta charset="utf-8">'
                   f'<meta name="viewport" content="width=device-width, initial-scale=1">'
                   f'<title>{title} · SU(3)</title>{STYLE}<main>'
                   f'<div class="mark">SU(3) LABS / RAMAN</div><h1>{title}</h1>{body}</main>'
                   ).encode()
        self.send_response(status)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Security-Policy",
                         "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; base-uri 'none'")
        self.send_header("Set-Cookie", f"su3_setup={TOKEN}; HttpOnly; SameSite=Strict; Path=/")
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        if not self.client_address[0].startswith("10.42.77."):
            self.send_error(403)
            return
        if self.headers.get("Host") != ADDRESS or urlsplit(self.path).path != "/":
            self.send_response(302)
            self.send_header("Location", f"http://{ADDRESS}/")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        message = f"<p>{html.escape(MESSAGE)}</p>" if MESSAGE else ""
        self.page("Connect to Wi-Fi", f"""{message}<p>Join this Raman node to your network.</p>
<form action="/connect" method="post">
<input type="hidden" name="csrf" value="{TOKEN}">
<label>Wi-Fi network name <input name="ssid" maxlength="32" required></label>
<label>Password <input name="password" type="password" maxlength="64" autocomplete="new-password"></label>
<details><summary>Enterprise Wi-Fi</summary>
<label>Username <input name="username" autocomplete="username"></label>
<label>RADIUS server domain <input name="domain" placeholder="radius.example.org"></label>
<small>PEAP with MSCHAPv2 and a system-trusted certificate.</small></details>
<label><input type="checkbox" name="open" value="1"> Open network</label>
<button type="submit">Connect</button></form>""")

    def do_POST(self):
        if (not self.client_address[0].startswith("10.42.77.")
                or self.headers.get("Host") != ADDRESS or self.path != "/connect"
                or self.headers.get("Origin") not in {None, f"http://{ADDRESS}"}):
            self.send_error(403)
            return
        try:
            size = int(self.headers.get("Content-Length", "0"))
            if not 0 < size <= 4096:
                raise ValueError("Invalid request size.")
            form = dict(parse_qs(self.rfile.read(size).decode(), max_num_fields=8,
                                 keep_blank_values=True))
            form = {key: values[0] for key, values in form.items() if len(values) == 1}
            cookies = self.headers.get("Cookie", "").split(";")
            cookie_ok = any(secrets.compare_digest(c.strip().encode(),
                                                   f"su3_setup={TOKEN}".encode()) for c in cookies)
            if not cookie_ok or not secrets.compare_digest(form.get("csrf", "").encode(), TOKEN.encode()):
                self.send_error(403)
                return
            ssid, security = credentials(form)
        except ValueError as err:
            self.page("Check Wi-Fi details", f"<p>{html.escape(str(err))}</p><a href='/'>Back</a>", 400)
            return
        if not BUSY.acquire(blocking=False):
            self.page("Connecting", "<p>A Wi-Fi connection attempt is already in progress.</p>", 409)
            return
        try:
            self.page("Connecting", f"<p>Reconnect your phone to your Wi-Fi, then open "
                      f"<a href='{SITE}'>{SITE}</a>.</p>")
        finally:
            threading.Thread(target=join, args=(self.server.iface, ssid, security), daemon=True).start()


def main():
    PROFILE_DIR.mkdir(mode=0o700, parents=True, exist_ok=True)
    nm("radio", "wifi", "on", check=False)
    server = None
    disconnected = 0
    while True:
        try:
            iface = wifi_device()
            if iface and BUSY.acquire(blocking=False):
                try:
                    active = active_profile(iface)
                    disconnected = disconnected + 1 if active in {"", "--"} else 0
                    if disconnected >= 4:
                        start_ap(iface)
                        active = SETUP
                    if active == SETUP and server is None:
                        server = ThreadingHTTPServer((ADDRESS, 80), Handler)
                        server.iface = iface
                        threading.Thread(target=server.serve_forever, daemon=True).start()
                    elif active != SETUP and server is not None:
                        server.shutdown()
                        server.server_close()
                        server = None
                finally:
                    BUSY.release()
        except (OSError, subprocess.SubprocessError):
            pass
        time.sleep(3)


if __name__ == "__main__":
    main()
