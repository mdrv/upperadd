use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};

use futures::channel::mpsc::{UnboundedSender, unbounded};
use futures::StreamExt;
use gpui::{
    App, AppContext, AsyncApp, Bounds, DisplayId, KeyBinding, WindowBounds,
    WindowBackgroundAppearance, WindowKind, WindowOptions, layer_shell::*, point, px, size,
};
use gpui_platform::application;
use log::warn;

use crate::cli::socket_path;
use crate::config::{Config, NamedOutput, OutputSpec};
use crate::overlay::{Hide, Overlay, OverlayGlobal};

/// Verbs the socket thread forwards into the gpui UI task.
enum Ipc {
    Toggle,
    Show,
    Stop,
}

pub fn run(cfg: Config) -> anyhow::Result<()> {
    let sock = socket_path();
    if sock.exists() {
        if UnixStream::connect(&sock).is_ok() {
            anyhow::bail!(
                "another upperadd daemon is already running (socket {})",
                sock.display()
            );
        }
        let _ = std::fs::remove_file(&sock); // stale socket from a crash
        log::info!("removed stale socket {}", sock.display());
    }
    let listener = UnixListener::bind(&sock)?;
    log::info!("listening on {}", sock.display());

    let (tx, mut rx) = unbounded::<Ipc>();

    application().run(move |cx: &mut App| {
        cx.bind_keys(vec![KeyBinding::new("escape", Hide, None)]);

        std::thread::spawn(move || accept_loop(listener, tx));

        // §16.9: ONE persistent window, created hidden (show: false), never
        // destroyed; Layer::Top per AGENTS.md fork rules.
        let display_id = resolve_display_id(cx, cfg.window.output);
        let options = WindowOptions {
            titlebar: None,
            show: false,
            app_id: Some("upperadd".into()),
            window_background: WindowBackgroundAppearance::Transparent,
            display_id,
            // §16.3: all four anchors fill the output — pass 0×0 bounds so
            // the compositor's anchors win.
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.), px(0.)),
                size: size(px(0.), px(0.)),
            })),
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "upperadd".into(),
                layer: Layer::Top,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: Some(px(-1.)),
                keyboard_interactivity: KeyboardInteractivity::None,
                ..Default::default()
            }),
            ..Default::default()
        };
        let handle = match cx.open_window(options, |_, cx| cx.new(|cx| Overlay::new(cfg, cx))) {
            Ok(handle) => handle,
            Err(err) => {
                log::error!("opening overlay window: {err:#}");
                cx.quit();
                return;
            }
        };
        cx.set_global(OverlayGlobal(handle));

        // §16.1: socket thread → unbounded channel → UI task; no blocking
        // calls inside async tasks.
        cx.spawn(async move |cx: &mut AsyncApp| {
            while let Some(msg) = rx.next().await {
                match msg {
                    Ipc::Stop => {
                        cx.update(|app| app.quit());
                        break;
                    }
                    Ipc::Toggle | Ipc::Show => {
                        let toggle = matches!(msg, Ipc::Toggle);
                        cx.update(|app| {
                            let handle = app.global::<OverlayGlobal>().0;
                            let _ = handle.update(app, |ov, window, cx| {
                                if toggle {
                                    ov.visible = !ov.visible;
                                } else {
                                    ov.visible = true;
                                }
                                ov.apply_visibility(window, cx);
                            });
                        });
                    }
                }
            }
        })
        .detach();
    });

    let _ = std::fs::remove_file(&sock);
    log::info!("socket removed, daemon stopped");
    Ok(())
}

fn accept_loop(listener: UnixListener, tx: UnboundedSender<Ipc>) {
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(_) => continue,
        };
        let mut line = String::new();
        if stream.read_to_string(&mut line).is_err() {
            continue;
        }
        let reply = match line.trim() {
            "toggle" => forward(&tx, Ipc::Toggle),
            "show" => forward(&tx, Ipc::Show),
            "stop" => forward(&tx, Ipc::Stop),
            "status" => Ok("running".into()),
            "reindex" => Ok("no index yet (M2)".into()),
            other => Err(format!("unknown verb {other:?}")),
        };
        let out = match reply {
            Ok(payload) => format!("ok {payload}\n"),
            Err(err) => format!("err {err}\n"),
        };
        let _ = stream.write_all(out.as_bytes());
    }
}

fn forward(tx: &UnboundedSender<Ipc>, msg: Ipc) -> Result<String, String> {
    tx.unbounded_send(msg).map(|_| String::new()).map_err(|_| "shutting down".into())
}

fn resolve_display_id(cx: &App, spec: OutputSpec) -> Option<DisplayId> {
    match spec {
        OutputSpec::Index(i) => cx.displays().get(i).map(|d| d.id()),
        OutputSpec::Named(NamedOutput::Primary) => cx.primary_display().map(|d| d.id()),
        OutputSpec::Named(NamedOutput::Cursor) => match hypr_cursor_display(cx) {
            Some(id) => Some(id),
            None => {
                warn!("could not resolve cursor output via hyprctl; falling back to primary");
                cx.primary_display().map(|d| d.id())
            }
        },
    }
}

/// gpui has no cursor-position API; under Hyprland, map the cursor to the
/// monitor containing it, then match that monitor's origin against gpui's
/// display bounds (both are global compositor coordinates).
fn hypr_cursor_display(cx: &App) -> Option<DisplayId> {
    let pos = run_capture("hyprctl", &["cursorpos"])?;
    let (x, y) = pos.trim().split_once(',')?;
    let (cx_, cy_): (f32, f32) = (x.trim().parse().ok()?, y.trim().parse().ok()?);

    let json = run_capture("hyprctl", &["monitors", "-j"])?;
    let monitors: serde_json::Value = serde_json::from_str(&json).ok()?;
    let monitor = monitors.as_array()?.into_iter().find(|m| {
        match (num(m, "x"), num(m, "y"), num(m, "width"), num(m, "height")) {
            (Some(x), Some(y), Some(w), Some(h)) => {
                cx_ >= x && cx_ < x + w && cy_ >= y && cy_ < y + h
            }
            _ => false,
        }
    })?;
    let (mx, my) = (num(monitor, "x")?, num(monitor, "y")?);

    cx.displays()
        .into_iter()
        .find(|d| {
            let b = d.bounds();
            (f32::from(b.origin.x) - mx).abs() < 1.0 && (f32::from(b.origin.y) - my).abs() < 1.0
        })
        .map(|d| d.id())
}

fn run_capture(program: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn num(value: &serde_json::Value, key: &str) -> Option<f32> {
    value.get(key)?.as_f64().map(|f| f as f32)
}
