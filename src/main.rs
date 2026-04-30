use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    #[arg(long, global = true)]
    socket: Option<PathBuf>,

    #[command(subcommand)]
    command: CommandLine,
}

#[derive(Subcommand, Debug)]
enum CommandLine {
    /// Run the long-lived history daemon.
    Daemon {
        #[arg(long)]
        state: Option<PathBuf>,
    },

    /// Preview the next or previous workspace in the active monitor's frozen history.
    Cycle { direction: Direction },

    /// Commit the currently previewed workspace and update history once.
    Commit,

    /// Cancel the current preview and return to the workspace where cycling started.
    Cancel,

    /// Print the daemon's current history state.
    History,

    /// Print example Hyprland bindings.
    Bindings,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Direction {
    Next,
    Prev,
}

impl Direction {
    fn increment(self) -> isize {
        match self {
            Direction::Next => 1,
            Direction::Prev => -1,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct HyprMonitor {
    name: String,
    focused: bool,
    #[serde(rename = "activeWorkspace")]
    active_workspace: HyprWorkspace,
}

#[derive(Clone, Debug, Deserialize)]
struct HyprWorkspace {
    id: i64,
    name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct HistoryEntry {
    monitor: String,
    workspace: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct PersistedState {
    history: Vec<HistoryEntry>,
}

#[derive(Clone, Debug)]
struct CycleSession {
    monitor: String,
    order: Vec<String>,
    index: usize,
    original_workspace: String,
    saved_history: Vec<HistoryEntry>,
}

#[derive(Debug)]
struct State {
    history: Vec<HistoryEntry>,
    session: Option<CycleSession>,
    state_path: PathBuf,
}

impl State {
    fn load(state_path: PathBuf) -> Result<Self> {
        let persisted = match fs::read_to_string(&state_path) {
            Ok(contents) => serde_json::from_str::<PersistedState>(&contents)
                .with_context(|| format!("failed to parse {}", state_path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => PersistedState::default(),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read {}", state_path.display()));
            }
        };

        Ok(Self {
            history: persisted.history,
            session: None,
            state_path,
        })
    }

    fn save(&self) -> Result<()> {
        if let Some(parent) = self.state_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let persisted = PersistedState {
            history: self.history.clone(),
        };
        let data = serde_json::to_vec_pretty(&persisted)?;
        fs::write(&self.state_path, data)
            .with_context(|| format!("failed to write {}", self.state_path.display()))
    }
}

#[derive(Clone, Debug)]
struct MonitorSnapshot {
    monitor: String,
    workspace: String,
    focused: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let socket = cli.socket.unwrap_or_else(default_command_socket);

    match cli.command {
        CommandLine::Daemon { state } => {
            run_daemon(socket, state.unwrap_or_else(default_state_path))
        }
        CommandLine::Cycle { direction } => send_command(&socket, &format!("cycle {direction:?}")),
        CommandLine::Commit => send_command(&socket, "commit"),
        CommandLine::Cancel => send_command(&socket, "cancel"),
        CommandLine::History => send_command(&socket, "history"),
        CommandLine::Bindings => {
            print_bindings();
            Ok(())
        }
    }
}

fn run_daemon(socket_path: PathBuf, state_path: PathBuf) -> Result<()> {
    let state = Arc::new(Mutex::new(State::load(state_path)?));
    {
        let mut state = state.lock().expect("state mutex poisoned");
        refresh_history(&mut state)?;
        state.save()?;
    }

    spawn_hyprland_event_thread(Arc::clone(&state));
    run_command_socket(socket_path, state)
}

fn spawn_hyprland_event_thread(state: Arc<Mutex<State>>) {
    thread::spawn(move || {
        loop {
            if let Err(error) = follow_hyprland_events(&state) {
                eprintln!("hypr-workspace-history: Hyprland event stream error: {error:#}");
                thread::sleep(Duration::from_secs(1));
            }
        }
    });
}

fn follow_hyprland_events(state: &Arc<Mutex<State>>) -> Result<()> {
    let socket = hyprland_event_socket()?;
    let stream = UnixStream::connect(&socket)
        .with_context(|| format!("failed to connect to {}", socket.display()))?;
    let reader = BufReader::new(stream);

    for line in reader.lines() {
        let line = line?;
        if !should_refresh_for_event(&line) {
            continue;
        }

        let mut state = state.lock().expect("state mutex poisoned");
        if state.session.is_some() {
            continue;
        }
        refresh_history(&mut state)?;
        state.save()?;
    }

    Ok(())
}

fn should_refresh_for_event(line: &str) -> bool {
    const EVENTS: &[&str] = &[
        "workspace>>",
        "focusedmon>>",
        "moveworkspace>>",
        "renameworkspace>>",
        "createworkspace>>",
        "destroyworkspace>>",
        "monitoradded>>",
        "monitorremoved>>",
    ];

    EVENTS.iter().any(|event| line.starts_with(event))
}

fn run_command_socket(socket_path: PathBuf, state: Arc<Mutex<State>>) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    if socket_path.exists() {
        fs::remove_file(&socket_path)
            .with_context(|| format!("failed to remove stale {}", socket_path.display()))?;
    }

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind {}", socket_path.display()))?;

    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                if let Err(error) = handle_client(&mut stream, &state) {
                    let _ = writeln!(stream, "error: {error:#}");
                }
            }
            Err(error) => eprintln!("hypr-workspace-history: client socket error: {error:#}"),
        }
    }

    Ok(())
}

fn handle_client(stream: &mut UnixStream, state: &Arc<Mutex<State>>) -> Result<()> {
    let mut request = String::new();
    stream.read_to_string(&mut request)?;
    let request = request.trim();

    let response = match request {
        "cycle Next" => {
            let mut state = state.lock().expect("state mutex poisoned");
            cycle(&mut state, Direction::Next)?
        }
        "cycle Prev" => {
            let mut state = state.lock().expect("state mutex poisoned");
            cycle(&mut state, Direction::Prev)?
        }
        "commit" => {
            let mut state = state.lock().expect("state mutex poisoned");
            commit(&mut state)?
        }
        "cancel" => {
            let mut state = state.lock().expect("state mutex poisoned");
            cancel(&mut state)?
        }
        "history" => {
            let state = state.lock().expect("state mutex poisoned");
            serde_json::to_string_pretty(&PersistedState {
                history: state.history.clone(),
            })?
        }
        other => bail!("unknown command {other:?}"),
    };

    writeln!(stream, "{response}")?;
    Ok(())
}

fn cycle(state: &mut State, direction: Direction) -> Result<String> {
    if state.session.is_none() {
        refresh_history(state)?;
        let snapshots = monitor_snapshots()?;
        let current = snapshots
            .iter()
            .find(|snapshot| snapshot.focused)
            .or_else(|| snapshots.first())
            .ok_or_else(|| anyhow!("Hyprland reported no monitors"))?;

        let mut order = history_for_monitor(&state.history, &current.monitor);
        if order.first() != Some(&current.workspace) {
            order.retain(|workspace| workspace != &current.workspace);
            order.insert(0, current.workspace.clone());
        }

        if order.len() < 2 {
            return Ok(format!(
                "monitor {} has no previous workspace history",
                current.monitor
            ));
        }

        state.session = Some(CycleSession {
            monitor: current.monitor.clone(),
            order,
            index: 1,
            original_workspace: current.workspace.clone(),
            saved_history: state.history.clone(),
        });
    }

    let session = state.session.as_mut().expect("session created above");
    let workspace = session.order[session.index].clone();
    let len = session.order.len() as isize;
    session.index = (session.index as isize + direction.increment()).rem_euclid(len) as usize;
    focus_workspace_on_monitor(&session.monitor, &workspace)?;

    Ok(format!(
        "preview monitor={} workspace={}",
        session.monitor, workspace
    ))
}

fn commit(state: &mut State) -> Result<String> {
    if let Some(session) = state.session.take() {
        state.history = session.saved_history;
        refresh_history(state)?;
        state.save()?;
        Ok("committed workspace history selection".to_string())
    } else {
        refresh_history(state)?;
        state.save()?;
        Ok("no active cycle session".to_string())
    }
}

fn cancel(state: &mut State) -> Result<String> {
    if let Some(session) = state.session.take() {
        state.history = session.saved_history;
        focus_workspace_on_monitor(&session.monitor, &session.original_workspace)?;
        state.save()?;
        Ok("cancelled workspace history selection".to_string())
    } else {
        Ok("no active cycle session".to_string())
    }
}

fn refresh_history(state: &mut State) -> Result<()> {
    let snapshots = monitor_snapshots()?;
    for snapshot in snapshots.iter().filter(|snapshot| !snapshot.focused) {
        update_last_for_monitor(
            &mut state.history,
            &snapshot.monitor,
            &snapshot.workspace,
            false,
        );
    }
    for snapshot in snapshots.iter().filter(|snapshot| snapshot.focused) {
        update_last_for_monitor(
            &mut state.history,
            &snapshot.monitor,
            &snapshot.workspace,
            true,
        );
    }
    Ok(())
}

fn update_last_for_monitor(
    history: &mut Vec<HistoryEntry>,
    monitor: &str,
    workspace: &str,
    force_front: bool,
) {
    let entry = HistoryEntry {
        monitor: monitor.to_string(),
        workspace: workspace.to_string(),
    };

    let already_first_for_monitor = history
        .iter()
        .find(|candidate| candidate.monitor == monitor)
        == Some(&entry);

    if already_first_for_monitor && !force_front {
        return;
    }

    history.retain(|candidate| candidate != &entry);
    history.insert(0, entry);
}

fn history_for_monitor(history: &[HistoryEntry], monitor: &str) -> Vec<String> {
    let mut workspaces = Vec::new();
    for entry in history.iter().filter(|entry| entry.monitor == monitor) {
        if !workspaces.contains(&entry.workspace) {
            workspaces.push(entry.workspace.clone());
        }
    }
    workspaces
}

fn monitor_snapshots() -> Result<Vec<MonitorSnapshot>> {
    let output = hyprctl_json(["-j", "monitors"])?;
    let monitors: Vec<HyprMonitor> =
        serde_json::from_slice(&output).context("failed to parse hyprctl -j monitors output")?;

    Ok(monitors
        .into_iter()
        .filter(|monitor| monitor.active_workspace.id > 0)
        .map(|monitor| MonitorSnapshot {
            monitor: monitor.name,
            workspace: monitor.active_workspace.name,
            focused: monitor.focused,
        })
        .collect())
}

fn focus_workspace_on_monitor(monitor: &str, workspace: &str) -> Result<()> {
    hyprctl(["dispatch", "focusmonitor", monitor])?;
    hyprctl(["dispatch", "workspace", &workspace_dispatch_arg(workspace)])?;
    Ok(())
}

fn workspace_dispatch_arg(workspace: &str) -> String {
    if workspace.parse::<i64>().is_ok() || workspace.starts_with("name:") {
        workspace.to_string()
    } else {
        format!("name:{workspace}")
    }
}

fn hyprctl<const N: usize>(args: [&str; N]) -> Result<()> {
    let output = Command::new("hyprctl")
        .args(args)
        .output()
        .context("failed to execute hyprctl")?;

    if !output.status.success() {
        bail!(
            "hyprctl failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(())
}

fn hyprctl_json<const N: usize>(args: [&str; N]) -> Result<Vec<u8>> {
    let output = Command::new("hyprctl")
        .args(args)
        .output()
        .context("failed to execute hyprctl")?;

    if !output.status.success() {
        bail!(
            "hyprctl failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(output.stdout)
}

fn send_command(socket: &Path, command: &str) -> Result<()> {
    let mut stream = UnixStream::connect(socket).with_context(|| {
        format!(
            "failed to connect to {}; is `hypr-workspace-history daemon` running?",
            socket.display()
        )
    })?;
    stream.write_all(command.as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    print!("{response}");
    Ok(())
}

fn hyprland_event_socket() -> Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("XDG_RUNTIME_DIR is not set"))?;
    let signature = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
        .context("HYPRLAND_INSTANCE_SIGNATURE is not set")?;
    Ok(runtime.join("hypr").join(signature).join(".socket2.sock"))
}

fn default_command_socket() -> PathBuf {
    runtime_dir().join("hypr-workspace-history.sock")
}

fn default_state_path() -> PathBuf {
    state_dir()
        .join("hypr-workspace-history")
        .join("state.json")
}

fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn print_bindings() {
    println!(
        r#"# Start once with your session:
exec-once = hypr-workspace-history daemon

# XMonad-style per-monitor workspace history cycling.
bind = SUPER, backslash, exec, hypr-workspace-history cycle next
bind = SUPER, slash, exec, hypr-workspace-history cycle prev

# Commit when the modifier is released. If this does not fire for your setup,
# bind the physical key that backs SUPER, e.g. Super_L.
bindr = SUPER, Super_L, exec, hypr-workspace-history commit

# Optional escape hatch for a submap or another cancel binding.
bind = SUPER SHIFT, backslash, exec, hypr-workspace-history cancel"#
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(monitor: &str, workspace: &str) -> HistoryEntry {
        HistoryEntry {
            monitor: monitor.to_string(),
            workspace: workspace.to_string(),
        }
    }

    #[test]
    fn monitor_history_filters_in_recency_order() {
        let history = vec![
            entry("DP-1", "3"),
            entry("HDMI-A-1", "8"),
            entry("DP-1", "2"),
            entry("DP-1", "1"),
        ];

        assert_eq!(history_for_monitor(&history, "DP-1"), vec!["3", "2", "1"]);
    }

    #[test]
    fn non_focused_monitor_update_preserves_existing_head() {
        let mut history = vec![entry("DP-1", "3"), entry("DP-1", "2")];

        update_last_for_monitor(&mut history, "DP-1", "3", false);

        assert_eq!(history, vec![entry("DP-1", "3"), entry("DP-1", "2")]);
    }

    #[test]
    fn focused_monitor_update_promotes_global_head() {
        let mut history = vec![
            entry("HDMI-A-1", "8"),
            entry("DP-1", "3"),
            entry("DP-1", "2"),
        ];

        update_last_for_monitor(&mut history, "DP-1", "3", true);

        assert_eq!(
            history,
            vec![
                entry("DP-1", "3"),
                entry("HDMI-A-1", "8"),
                entry("DP-1", "2"),
            ]
        );
    }

    #[test]
    fn workspace_dispatch_arg_uses_name_prefix_for_named_workspaces() {
        assert_eq!(workspace_dispatch_arg("1"), "1");
        assert_eq!(workspace_dispatch_arg("mail"), "name:mail");
        assert_eq!(workspace_dispatch_arg("name:mail"), "name:mail");
    }
}
