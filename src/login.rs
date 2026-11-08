//! Logging in to live.warthunder.com.
//!
//! The site uses Gaijin's single sign-on page (login.gaijin.net, with its own 2FA and captcha),
//! which a native client can't drive. So the app opens that page in a real browser engine
//! (WebView2, built into Windows 10/11) in a small second process (`WTArmoryManager.exe --login`;
//! egui and the webview each want their own event loop). The user logs in there themselves; the
//! app never sees the password. When the site reports a logged-in user, the window reads the
//! live.warthunder.com cookies, saves them to `session.json` and closes.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const ARG: &str = "--login";
const SESSION_FILE: &str = "session.json";

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct Session {
    /// "name=value; name2=value2" for live.warthunder.com.
    pub cookies: String,
    pub user_id: u64,
    #[serde(default)]
    pub nickname: String,
    #[serde(default)]
    pub avatar: Option<String>,
}

pub fn load() -> Option<Session> {
    let s: Session = crate::storage::load(SESSION_FILE);
    (s.user_id != 0 && !s.cookies.is_empty()).then_some(s)
}

pub fn save(s: &Session) -> Result<(), String> {
    crate::storage::save(SESSION_FILE, s)
}

fn webview_dir() -> PathBuf {
    crate::storage::app_dir().join("webview")
}

/// Forgets the session: the saved cookies and the login window's browser profile.
pub fn logout() {
    let _ = std::fs::remove_file(crate::storage::app_dir().join(SESSION_FILE));
    let _ = std::fs::remove_dir_all(webview_dir());
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The site's own login entry point; it redirects to Gaijin's login page and back.
fn login_url() -> String {
    format!("{}/auth/auth/?return={}", crate::api::BASE, base64(format!("{}/feed/", crate::api::BASE).as_bytes()))
}

/// Starts the login window as a child process and waits for it. Returns the new session, if the
/// user finished logging in.
pub fn run_child() -> Result<Option<Session>, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let status = std::process::Command::new(exe)
        .arg(ARG)
        .status()
        .map_err(|e| format!("Could not open the login window: {e}"))?;
    if !status.success() {
        return Err("The login window could not start. WebView2 (part of Windows 10/11 and Microsoft Edge) is needed.".into());
    }
    Ok(load())
}

enum Event {
    LoggedIn(u64),
}

/// Entry point of the `--login` process. Exits the process when done.
pub fn window_main() -> ! {
    use tao::event::{Event as TaoEvent, WindowEvent};
    use tao::event_loop::{ControlFlow, EventLoopBuilder};
    use tao::window::WindowBuilder;

    let event_loop = EventLoopBuilder::<Event>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = match WindowBuilder::new()
        .with_title("Log in to live.warthunder.com - WT Armory Manager")
        .with_inner_size(tao::dpi::LogicalSize::new(560.0, 780.0))
        .build(&event_loop)
    {
        Ok(w) => w,
        Err(_) => std::process::exit(2),
    };

    // Every page on live.warthunder.com carries `live_wt.user_id` (0 when logged out).
    let script = r#"
        window.addEventListener('DOMContentLoaded', function () {
            try {
                if (location.hostname === 'live.warthunder.com' && location.pathname.indexOf('/auth/') !== 0) {
                    var id = (window.live_wt && window.live_wt.user_id) || 0;
                    window.ipc.postMessage(String(id));
                }
            } catch (e) {}
        });
    "#;
    let mut context = wry::WebContext::new(Some(webview_dir()));
    let webview = wry::WebViewBuilder::new_with_web_context(&mut context)
        .with_url(login_url())
        .with_initialization_script(script)
        .with_ipc_handler(move |req| {
            if let Ok(id) = req.body().trim().parse::<u64>() {
                if id != 0 {
                    let _ = proxy.send_event(Event::LoggedIn(id));
                }
            }
        })
        .build(&window);
    let webview = match webview {
        Ok(w) => w,
        Err(_) => std::process::exit(3),
    };

    let mut done = false;
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            TaoEvent::UserEvent(Event::LoggedIn(user_id)) if !done => {
                // Read the cookies here, outside the webview callbacks (WebView2 answers async).
                let cookies = webview.cookies_for_url(crate::api::BASE).unwrap_or_default();
                let header = cookies.iter().map(|c| format!("{}={}", c.name(), c.value())).collect::<Vec<_>>().join("; ");
                if !header.is_empty() {
                    done = true;
                    let _ = save(&Session { cookies: header, user_id, nickname: String::new(), avatar: None });
                    *flow = ControlFlow::Exit;
                }
            }
            TaoEvent::WindowEvent { event: WindowEvent::CloseRequested, .. } => *flow = ControlFlow::Exit,
            _ => {}
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_matches_reference() {
        assert_eq!(super::base64(b"https://live.warthunder.com/feed/"), "aHR0cHM6Ly9saXZlLndhcnRodW5kZXIuY29tL2ZlZWQv");
        assert_eq!(super::base64(b"ab"), "YWI=");
        assert_eq!(super::base64(b"a"), "YQ==");
    }
}
