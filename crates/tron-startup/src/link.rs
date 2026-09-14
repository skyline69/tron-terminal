//! Commands to the tron window the startup screen runs in: its startup shader
//! and the shader's parameters. Sent as `OSC 7777 ; token ; payload` and
//! ignored by tron without the token it passed in the environment.

use std::io::Write;
use std::time::Duration;

/// Longest gap between commands while the shader runs.
const KEEPALIVE: Duration = Duration::from_secs(1);

pub struct Link<W: Write> {
    token: Option<String>,
    out: W,
    shader_on: bool,
    last_params: String,
    /// When anything was last sent, for keepalives.
    last_sent: std::time::Instant,
    /// A preview was sent and not yet restored or committed.
    previewing: bool,
}

impl<W: Write> Link<W> {
    /// Without a token (not running inside tron) nothing is sent.
    pub fn new(token: Option<String>, out: W) -> Self {
        Self {
            token: token.filter(|t| !t.is_empty()),
            out,
            shader_on: false,
            last_params: String::new(),
            previewing: false,
            last_sent: std::time::Instant::now(),
        }
    }

    pub fn shader_on(&mut self, scene: u32) {
        self.shader_on = true;
        // Parameters were reset when the shader went off; send them again.
        self.last_params.clear();
        self.send(&format!("shader=on,scene={scene}"));
    }

    /// Switches the shader to another scene.
    pub fn scene(&mut self, scene: u32) {
        if self.shader_on {
            self.send(&format!("scene={scene}"));
        }
    }

    pub fn shader_off(&mut self) {
        if std::mem::take(&mut self.shader_on) {
            self.send("params=0:0:0:0,shader=off");
        }
    }

    /// Sends shader parameters when they changed since the last call. Call every
    /// frame: without changes it sends a keepalive once a second, because tron
    /// turns the shader off when the screen goes quiet (for example after a crash).
    pub fn params(&mut self, params: [f32; 4]) {
        let payload = format!("params={:.3}:{:.3}:{:.3}:{:.3}", params[0], params[1], params[2], params[3]);
        if self.shader_on && payload != self.last_params {
            self.send(&payload);
            self.last_params = payload;
        } else if self.shader_on && self.last_sent.elapsed() >= KEEPALIVE {
            self.send("alive");
        }
    }

    /// Shows the window with `toml` layered over the saved configuration.
    pub fn preview(&mut self, toml: &str) {
        use base64::Engine;
        self.previewing = true;
        self.send(&format!("preview={}", base64::engine::general_purpose::STANDARD.encode(toml)));
    }

    /// Drops the preview and returns to the saved configuration.
    pub fn restore(&mut self) {
        if std::mem::take(&mut self.previewing) {
            self.send("restore");
        }
    }

    /// Stops previewing after the configuration was saved.
    pub fn commit(&mut self) {
        self.previewing = false;
        self.send("commit");
    }

    fn send(&mut self, payload: &str) {
        let Some(token) = &self.token else { return };
        let _ = write!(self.out, "\x1b]7777;{token};{payload}\x07");
        let _ = self.out.flush();
        self.last_sent = std::time::Instant::now();
    }
}

impl<W: Write> Drop for Link<W> {
    fn drop(&mut self) {
        self.restore();
        self.shader_off();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sends_only_with_a_token_and_only_changes() {
        let mut out = Vec::new();
        {
            let mut link = Link::new(Some("t0k".into()), &mut out);
            link.shader_on(1);
            link.params([1.0, 0.5, 0.0, 0.25]);
            link.params([1.0, 0.5, 0.0, 0.25]);
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\x1b]7777;t0k;shader=on,scene=1\x07\x1b]7777;t0k;params=1.000:0.500:0.000:0.250\x07\
             \x1b]7777;t0k;params=0:0:0:0,shader=off\x07"
        );
        let mut out = Vec::new();
        {
            let mut link = Link::new(Some("t".into()), &mut out);
            link.preview("theme = \"nord\"");
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\x1b]7777;t;preview=dGhlbWUgPSAibm9yZCI=\x07\x1b]7777;t;restore\x07"
        );
        let mut out = Vec::new();
        {
            let mut link = Link::new(Some("t".into()), &mut out);
            link.shader_on(2);
            link.params([1.0; 4]);
            link.last_sent -= KEEPALIVE;
            link.params([1.0; 4]);
        }
        assert!(String::from_utf8(out).unwrap().contains("\x1b]7777;t;alive\x07"), "keepalive without changes");
        let mut silent = Vec::new();
        Link::new(None, &mut silent).shader_on(1);
        assert!(silent.is_empty());
    }
}
