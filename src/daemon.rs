use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::rc::Rc;

use futures::channel::mpsc::{UnboundedSender, unbounded};
use futures::channel::oneshot;
use futures::StreamExt;
use gpui::{
    App, AppContext, AsyncApp, Bounds, DisplayId, Pixels, Point, PlatformDisplay,
    WindowBounds, WindowBackgroundAppearance, WindowKind, WindowOptions, layer_shell::*, point,
    px, size,
};
use gpui_platform::application;
use log::warn;

use crate::cli::socket_path;
use crate::config::{Config, NamedOutput, OutputSpec};
use crate::overlay::{Overlay, OverlayGlobal};
use crate::worker::{self, IndexCmd};

/// Verbs the socket thread forwards into the gpui UI task.
enum Ipc {
    Toggle,
    Show,
    Stop,
    PinTest,
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
    let index_tx = worker::spawn(cfg.notes.dir.clone(), cfg.sections.separator);
    let ui_index_tx = index_tx.clone();

    application().run(move |cx: &mut App| {
        std::thread::spawn(move || accept_loop(listener, tx, index_tx));

        // §16.9: ONE persistent window, created hidden (show: false), never
        // destroyed; Layer::Top per AGENTS.md fork rules.
        let (display_id, output_origin) = match resolve_display(cx, cfg.window.output) {
            Some((id, origin)) => (Some(id), origin),
            None => (None, point(px(0.), px(0.))),
        };
        let overlay_cfg = cfg.clone();
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
        let handle = match cx.open_window(options, |_, cx| {
            cx.new(|cx| Overlay::new(overlay_cfg, ui_index_tx.clone(), output_origin, cx))
        }) {
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
            let mut pins = 0usize;
            while let Some(msg) = rx.next().await {
                match msg {
                    Ipc::Stop => {
                        cx.update(|app| app.quit());
                        break;
                    }
                    Ipc::PinTest => {
                        pins += 1;
                        let index = pins;
                        let origin = output_origin;
                        cx.update(|app| {
                            if let Err(err) = crate::sticky::spawn(
                                app,
                                &cfg,
                                format!("Pinned test #{index}"),
                                index,
                                origin,
                            ) {
                                warn!("spawning sticky: {err:#}");
                            }
                        });
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

fn accept_loop(
    listener: UnixListener,
    tx: UnboundedSender<Ipc>,
    index_tx: UnboundedSender<IndexCmd>,
) {
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
            "status" => ask_index(&index_tx, |resp| IndexCmd::Stats { resp }),
            "reindex" => index_tx
                .unbounded_send(IndexCmd::Reindex)
                .map(|_| "reindex started".to_string())
                .map_err(|_| "index worker gone".to_string()),
            "pin-test" => forward(&tx, Ipc::PinTest),
            other => Err(format!("unknown verb {other:?}")),
        };
        let out = match reply {
            Ok(payload) => format!("ok {payload}\n"),
            Err(err) => format!("err {err}\n"),
        };
        let _ = stream.write_all(out.as_bytes());
    }
}

/// Send a command to the index worker and wait for its reply.
fn ask_index(
    index_tx: &UnboundedSender<IndexCmd>,
    cmd: impl FnOnce(oneshot::Sender<String>) -> IndexCmd,
) -> Result<String, String> {
    let (resp_tx, resp_rx) = oneshot::channel();
    index_tx
        .unbounded_send(cmd(resp_tx))
        .map_err(|_| "index worker gone".to_string())?;
    futures::executor::block_on(resp_rx).map_err(|_| "index worker dropped the request".into())
}

fn forward(tx: &UnboundedSender<Ipc>, msg: Ipc) -> Result<String, String> {
    tx.unbounded_send(msg).map(|_| String::new()).map_err(|_| "shutting down".into())
}

/// The display a new window should land on: (id, layout origin).
fn resolve_display(cx: &App, spec: OutputSpec) -> Option<(DisplayId, Point<Pixels>)> {
    let display = match spec {
        OutputSpec::Index(i) => cx.displays().into_iter().nth(i)?,
        OutputSpec::Named(NamedOutput::Primary) => cx.primary_display()?,
        OutputSpec::Named(NamedOutput::Cursor) => match hypr_cursor_display(cx) {
            Some(display) => display,
            None => {
                warn!("could not resolve cursor output via hyprctl; falling back to primary");
                cx.primary_display()?
            }
        },
    };
    Some((display.id(), display.bounds().origin))
}

/// Global cursor position (layout coordinates) via Hyprland IPC — ground
/// truth for sticky drags, independent of surface-local origin shifts.
/// None when not under Hyprland.
pub fn hypr_cursor_global() -> Option<Point<Pixels>> {
    let pos = run_capture("hyprctl", &["cursorpos"])?;
    let (x, y) = pos.trim().split_once(',')?;
    Some(point(px(x.trim().parse::<f32>().ok()?), px(y.trim().parse::<f32>().ok()?)))
}

/// gpui has no cursor-position API; under Hyprland, map the cursor to the
/// monitor containing it, then match that monitor's origin against gpui's
/// display bounds (both are global compositor coordinates).
fn hypr_cursor_display(cx: &App) -> Option<Rc<dyn PlatformDisplay>> {
    let cursor = hypr_cursor_global()?;
    let (cx_, cy_) = (f32::from(cursor.x), f32::from(cursor.y));

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

    let displays = cx.displays();
    displays.into_iter().find(|d| {
        let b = d.bounds();
        (f32::from(b.origin.x) - mx).abs() < 1.0 && (f32::from(b.origin.y) - my).abs() < 1.0
    })
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
