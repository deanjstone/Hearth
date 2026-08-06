// Ported from electron/main/terminal/pty.ts. One real shell per terminal id
// (the renderer generates an id per panel), spawned in the session's
// workspace cwd. Output streams to the renderer; input/resizes flow back.
// `node-pty` -> `portable-pty` (spec #26's Implementation Decisions).
//
// `PtyProcess`/`PtySpawner` are the DI seam spec #26's testing decisions call
// for ("a `PtyProcess` trait... tested with fakes, not real subprocesses/OS
// calls") — `TerminalManager`'s id-tracking/lifecycle logic is tested against
// a `FakePtySpawner` below; `RealPtySpawner` (the portable-pty-backed impl)
// gets its own narrower tests using a real spawned shell.

use super::login_path::{LoginPathResolver, ShellQuery};
use crate::agents::child_env::{build_child_env, BuildChildEnvOptions};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

/// The user's shell for a spawned terminal. Windows branch is unreachable on
/// this port's WSL2/Linux-only target (spec #26) but kept for line-for-line
/// fidelity with the TS original, matching the precedent set in Chunk 4's
/// `startup_check.rs`.
pub fn default_shell(env: &HashMap<String, String>) -> String {
    if cfg!(target_os = "windows") {
        env.get("COMSPEC")
            .cloned()
            .unwrap_or_else(|| "powershell.exe".to_string())
    } else {
        env.get("SHELL")
            .cloned()
            .unwrap_or_else(|| "/bin/zsh".to_string())
    }
}

/// A single spawned pty process, abstracted so `TerminalManager` doesn't
/// need a real OS pty to be tested.
pub trait PtyProcess: Send {
    fn write(&self, data: &str);
    fn resize(&self, cols: u16, rows: u16);
    fn kill(&self);
}

/// Spawns a `PtyProcess`, streaming output via `on_data` and firing `on_exit`
/// exactly once when the process ends (naturally or via `kill`).
pub trait PtySpawner: Send + Sync {
    #[allow(clippy::too_many_arguments)]
    fn spawn(
        &self,
        shell: &str,
        cwd: &Path,
        cols: u16,
        rows: u16,
        env: HashMap<String, String>,
        on_data: Box<dyn Fn(String) + Send>,
        on_exit: Box<dyn FnOnce() + Send>,
    ) -> Result<Box<dyn PtyProcess>, String>;
}

/// The real `portable-pty`-backed spawner.
pub struct RealPtySpawner;

impl PtySpawner for RealPtySpawner {
    fn spawn(
        &self,
        shell: &str,
        cwd: &Path,
        cols: u16,
        rows: u16,
        env: HashMap<String, String>,
        on_data: Box<dyn Fn(String) + Send>,
        on_exit: Box<dyn FnOnce() + Send>,
    ) -> Result<Box<dyn PtyProcess>, String> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())?;

        let mut cmd = CommandBuilder::new(shell);
        cmd.cwd(cwd);
        // `env` is already the FULL intended child env (buildChildEnv's
        // scrub-then-merge result) — env_clear() so nothing from Hearth's
        // own ambient process env leaks in underneath it, matching TS's
        // `pty.spawn(shell, [], { env })`, which replaces rather than
        // overlays.
        cmd.env_clear();
        for (k, v) in &env {
            cmd.env(k, v);
        }

        let mut child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
        let killer = child.clone_killer();
        // Must drop the slave side before the master can see EOF on process
        // exit — holding it open past spawn would leave the reader thread
        // blocked forever after the child actually exits.
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;

        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => on_data(String::from_utf8_lossy(&buf[..n]).into_owned()),
                }
            }
        });

        thread::spawn(move || {
            let _ = child.wait();
            on_exit();
        });

        Ok(Box::new(RealPtyProcess {
            master: pair.master,
            writer: Mutex::new(writer),
            killer: Mutex::new(killer),
        }))
    }
}

struct RealPtyProcess {
    master: Box<dyn MasterPty + Send>,
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
}

impl PtyProcess for RealPtyProcess {
    fn write(&self, data: &str) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(data.as_bytes());
        }
    }

    fn resize(&self, cols: u16, rows: u16) {
        // A resize can race a process that already exited (the renderer's
        // ResizeObserver can fire mid-teardown) — swallow the error,
        // matching pty.ts's own try/catch here.
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    fn kill(&self) {
        if let Ok(mut k) = self.killer.lock() {
            let _ = k.kill();
        }
    }
}

type DataCb = Arc<dyn Fn(&str, &str) + Send + Sync>;
type ExitCb = Arc<dyn Fn(&str) + Send + Sync>;

/// One real shell per terminal id. Owns the `LoginPathResolver` + base env
/// snapshot needed to build each spawn's env, matching `pty.ts`'s own inline
/// `buildChildEnv(env, { PATH: loginPath(), TERM: ... }, { scrubInheritedKeys: true })`
/// call.
pub struct TerminalManager<S: PtySpawner, Q: ShellQuery> {
    spawner: S,
    login: LoginPathResolver<Q>,
    base_env: HashMap<String, String>,
    terms: Arc<Mutex<HashMap<String, Box<dyn PtyProcess>>>>,
    on_data: DataCb,
    on_exit: ExitCb,
}

impl<S: PtySpawner, Q: ShellQuery> TerminalManager<S, Q> {
    pub fn new(
        spawner: S,
        login: LoginPathResolver<Q>,
        base_env: HashMap<String, String>,
        on_data: impl Fn(&str, &str) + Send + Sync + 'static,
        on_exit: impl Fn(&str) + Send + Sync + 'static,
    ) -> Self {
        Self {
            spawner,
            login,
            base_env,
            terms: Arc::new(Mutex::new(HashMap::new())),
            on_data: Arc::new(on_data),
            on_exit: Arc::new(on_exit),
        }
    }

    fn lock_terms(&self) -> std::sync::MutexGuard<'_, HashMap<String, Box<dyn PtyProcess>>> {
        self.terms
            .lock()
            .expect("terminal manager's terms mutex poisoned")
    }

    pub fn create(&self, id: &str, cwd: &Path, cols: u16, rows: u16) -> Result<(), String> {
        if self.lock_terms().contains_key(id) {
            return Ok(());
        }

        let shell = default_shell(&self.base_env);
        let inherited_path = self.base_env.get("PATH").cloned().unwrap_or_default();
        let mut extra = HashMap::new();
        extra.insert(
            "PATH".to_string(),
            self.login.login_path(&inherited_path, &shell),
        );
        extra.insert("TERM".to_string(), "xterm-256color".to_string());
        let env = build_child_env(
            &self.base_env,
            &extra,
            BuildChildEnvOptions {
                scrub_inherited_keys: true,
            },
        );

        let terms = self.terms.clone();
        let on_exit_cb = self.on_exit.clone();
        let id_for_exit = id.to_string();
        let id_for_cleanup = id.to_string();
        let on_data_cb = self.on_data.clone();
        let id_for_data = id.to_string();

        let proc = self.spawner.spawn(
            &shell,
            cwd,
            cols,
            rows,
            env,
            Box::new(move |data| on_data_cb(&id_for_data, &data)),
            Box::new(move || {
                on_exit_cb(&id_for_exit);
                terms
                    .lock()
                    .expect("terminal manager's terms mutex poisoned")
                    .remove(&id_for_cleanup);
            }),
        )?;

        self.lock_terms().insert(id.to_string(), proc);
        Ok(())
    }

    pub fn write(&self, id: &str, data: &str) {
        if let Some(proc) = self.lock_terms().get(id) {
            proc.write(data);
        }
    }

    pub fn resize(&self, id: &str, cols: u16, rows: u16) {
        if let Some(proc) = self.lock_terms().get(id) {
            proc.resize(cols, rows);
        }
    }

    pub fn kill(&self, id: &str) {
        if let Some(proc) = self.lock_terms().remove(id) {
            proc.kill();
        }
    }

    pub fn dispose_all(&self) {
        let ids: Vec<String> = self.lock_terms().keys().cloned().collect();
        for id in ids {
            self.kill(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::login_path::RealShellQuery;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    // --- default_shell ---

    #[test]
    fn default_shell_unix_uses_env_shell() {
        let mut env = HashMap::new();
        env.insert("SHELL".to_string(), "/bin/bash".to_string());
        if !cfg!(target_os = "windows") {
            assert_eq!(default_shell(&env), "/bin/bash");
        }
    }

    #[test]
    fn default_shell_unix_falls_back_to_zsh() {
        if !cfg!(target_os = "windows") {
            assert_eq!(default_shell(&HashMap::new()), "/bin/zsh");
        }
    }

    // --- TerminalManager, against a fake PtySpawner ---

    struct FakeProc {
        writes: Arc<Mutex<Vec<String>>>,
        resizes: Arc<Mutex<Vec<(u16, u16)>>>,
        killed: Arc<Mutex<bool>>,
    }

    impl PtyProcess for FakeProc {
        fn write(&self, data: &str) {
            self.writes.lock().unwrap().push(data.to_string());
        }
        fn resize(&self, cols: u16, rows: u16) {
            self.resizes.lock().unwrap().push((cols, rows));
        }
        fn kill(&self) {
            *self.killed.lock().unwrap() = true;
        }
    }

    /// Captures every spawn call and hands the test a way to fire the
    /// on_data/on_exit callbacks it was given, standing in for a real OS
    /// process's async output/exit.
    type ProcHandles = (
        Arc<Mutex<Vec<String>>>,
        Arc<Mutex<Vec<(u16, u16)>>>,
        Arc<Mutex<bool>>,
    );
    type CapturedSpawn = (String, Box<dyn Fn(String) + Send>, Box<dyn FnOnce() + Send>);

    struct FakeSpawner {
        spawn_count: AtomicUsize,
        fail_next: Mutex<bool>,
        captured: Mutex<Vec<CapturedSpawn>>,
        last_proc: Mutex<Option<ProcHandles>>,
    }

    impl FakeSpawner {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                spawn_count: AtomicUsize::new(0),
                fail_next: Mutex::new(false),
                captured: Mutex::new(Vec::new()),
                last_proc: Mutex::new(None),
            })
        }
    }

    impl PtySpawner for Arc<FakeSpawner> {
        fn spawn(
            &self,
            _shell: &str,
            _cwd: &Path,
            _cols: u16,
            _rows: u16,
            _env: HashMap<String, String>,
            on_data: Box<dyn Fn(String) + Send>,
            on_exit: Box<dyn FnOnce() + Send>,
        ) -> Result<Box<dyn PtyProcess>, String> {
            self.spawn_count.fetch_add(1, Ordering::SeqCst);
            if std::mem::replace(&mut *self.fail_next.lock().unwrap(), false) {
                return Err("spawn failed".to_string());
            }
            self.captured
                .lock()
                .unwrap()
                .push(("spawned".to_string(), on_data, on_exit));
            let writes = Arc::new(Mutex::new(Vec::new()));
            let resizes = Arc::new(Mutex::new(Vec::new()));
            let killed = Arc::new(Mutex::new(false));
            *self.last_proc.lock().unwrap() =
                Some((writes.clone(), resizes.clone(), killed.clone()));
            Ok(Box::new(FakeProc {
                writes,
                resizes,
                killed,
            }))
        }
    }

    struct NoopShellQuery;
    impl ShellQuery for NoopShellQuery {
        fn resolve_login_path(&self, _shell: &str) -> Option<String> {
            None
        }
        fn which(&self, _name: &str, _env: &HashMap<String, String>) -> bool {
            false
        }
    }

    type ManagerFixture = (
        TerminalManager<Arc<FakeSpawner>, NoopShellQuery>,
        mpsc::Receiver<(String, String)>,
        mpsc::Receiver<String>,
    );

    fn manager_with(spawner: Arc<FakeSpawner>) -> ManagerFixture {
        let (data_tx, data_rx) = mpsc::channel();
        let (exit_tx, exit_rx) = mpsc::channel();
        let manager = TerminalManager::new(
            spawner,
            LoginPathResolver::new(NoopShellQuery, false),
            HashMap::new(),
            move |id, data| {
                let _ = data_tx.send((id.to_string(), data.to_string()));
            },
            move |id| {
                let _ = exit_tx.send(id.to_string());
            },
        );
        (manager, data_rx, exit_rx)
    }

    #[test]
    fn create_spawns_exactly_once_for_a_new_id() {
        let spawner = FakeSpawner::new();
        let (manager, _data_rx, _exit_rx) = manager_with(spawner.clone());
        manager.create("t1", Path::new("/tmp"), 80, 24).unwrap();
        assert_eq!(spawner.spawn_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn create_is_a_noop_for_an_id_already_tracked() {
        let spawner = FakeSpawner::new();
        let (manager, _data_rx, _exit_rx) = manager_with(spawner.clone());
        manager.create("t1", Path::new("/tmp"), 80, 24).unwrap();
        manager.create("t1", Path::new("/tmp"), 80, 24).unwrap();
        assert_eq!(spawner.spawn_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn create_propagates_a_spawn_error() {
        let spawner = FakeSpawner::new();
        *spawner.fail_next.lock().unwrap() = true;
        let (manager, _data_rx, _exit_rx) = manager_with(spawner.clone());
        let result = manager.create("t1", Path::new("/tmp"), 80, 24);
        assert_eq!(result, Err("spawn failed".to_string()));
    }

    #[test]
    fn on_data_is_forwarded_with_the_terminal_id() {
        let spawner = FakeSpawner::new();
        let (manager, data_rx, _exit_rx) = manager_with(spawner.clone());
        manager.create("t1", Path::new("/tmp"), 80, 24).unwrap();
        let (_, on_data, _) = spawner.captured.lock().unwrap().pop().unwrap();
        on_data("hello".to_string());
        assert_eq!(
            data_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            ("t1".to_string(), "hello".to_string())
        );
    }

    #[test]
    fn on_exit_is_forwarded_and_removes_the_terminal_from_tracking() {
        let spawner = FakeSpawner::new();
        let (manager, _data_rx, exit_rx) = manager_with(spawner.clone());
        manager.create("t1", Path::new("/tmp"), 80, 24).unwrap();
        let (_, _, on_exit) = spawner.captured.lock().unwrap().pop().unwrap();
        on_exit();
        assert_eq!(exit_rx.recv_timeout(Duration::from_secs(1)).unwrap(), "t1");
        // A second create() for the same id spawns again — the exit cleaned
        // up the tracking entry.
        manager.create("t1", Path::new("/tmp"), 80, 24).unwrap();
        assert_eq!(spawner.spawn_count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn write_reaches_the_right_process_only() {
        let spawner = FakeSpawner::new();
        let (manager, _data_rx, _exit_rx) = manager_with(spawner.clone());
        manager.create("t1", Path::new("/tmp"), 80, 24).unwrap();
        manager.write("t1", "echo hi\n");
        manager.write("nonexistent", "should be dropped silently");
        // No panic on the unknown id is the assertion here.
    }

    #[test]
    fn resize_reaches_the_right_process_only() {
        let spawner = FakeSpawner::new();
        let (manager, _data_rx, _exit_rx) = manager_with(spawner.clone());
        manager.create("t1", Path::new("/tmp"), 80, 24).unwrap();
        manager.resize("t1", 100, 40);
        manager.resize("nonexistent", 1, 1); // dropped silently, no panic

        let (_, resizes, _) = spawner.last_proc.lock().unwrap().clone().unwrap();
        assert_eq!(*resizes.lock().unwrap(), vec![(100, 40)]);
    }

    #[test]
    fn write_is_recorded_on_the_right_process() {
        let spawner = FakeSpawner::new();
        let (manager, _data_rx, _exit_rx) = manager_with(spawner.clone());
        manager.create("t1", Path::new("/tmp"), 80, 24).unwrap();
        manager.write("t1", "echo hi\n");

        let (writes, _, _) = spawner.last_proc.lock().unwrap().clone().unwrap();
        assert_eq!(*writes.lock().unwrap(), vec!["echo hi\n".to_string()]);
    }

    #[test]
    fn kill_marks_the_process_killed() {
        let spawner = FakeSpawner::new();
        let (manager, _data_rx, _exit_rx) = manager_with(spawner.clone());
        manager.create("t1", Path::new("/tmp"), 80, 24).unwrap();
        manager.kill("t1");

        let (_, _, killed) = spawner.last_proc.lock().unwrap().clone().unwrap();
        assert!(*killed.lock().unwrap());
    }

    #[test]
    fn kill_removes_tracking_so_a_later_create_spawns_again() {
        let spawner = FakeSpawner::new();
        let (manager, _data_rx, _exit_rx) = manager_with(spawner.clone());
        manager.create("t1", Path::new("/tmp"), 80, 24).unwrap();
        manager.kill("t1");
        manager.create("t1", Path::new("/tmp"), 80, 24).unwrap();
        assert_eq!(spawner.spawn_count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn kill_of_an_unknown_id_does_not_panic() {
        let spawner = FakeSpawner::new();
        let (manager, _data_rx, _exit_rx) = manager_with(spawner.clone());
        manager.kill("never-created");
    }

    #[test]
    fn dispose_all_kills_every_tracked_terminal() {
        let spawner = FakeSpawner::new();
        let (manager, _data_rx, _exit_rx) = manager_with(spawner.clone());
        manager.create("t1", Path::new("/tmp"), 80, 24).unwrap();
        manager.create("t2", Path::new("/tmp"), 80, 24).unwrap();
        manager.dispose_all();
        assert!(manager.lock_terms().is_empty());
    }

    // --- RealPtySpawner, against a real spawned shell ---

    #[test]
    fn real_spawner_round_trips_a_command_through_the_pty() {
        let spawner = RealPtySpawner;
        let mut env = HashMap::new();
        env.insert(
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_default(),
        );
        env.insert("TERM".to_string(), "xterm-256color".to_string());

        let (data_tx, data_rx) = mpsc::channel::<String>();
        let (exit_tx, exit_rx) = mpsc::channel::<()>();

        let proc = spawner
            .spawn(
                "/bin/sh",
                &std::env::temp_dir(),
                80,
                24,
                env,
                Box::new(move |data| {
                    let _ = data_tx.send(data);
                }),
                Box::new(move || {
                    let _ = exit_tx.send(());
                }),
            )
            .expect("spawn should succeed");

        proc.write("echo hello-from-pty\n");

        let mut collected = String::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline && !collected.contains("hello-from-pty") {
            if let Ok(chunk) = data_rx.recv_timeout(Duration::from_millis(200)) {
                collected.push_str(&chunk);
            }
        }
        assert!(collected.contains("hello-from-pty"), "got: {collected:?}");

        proc.write("exit\n");
        assert!(
            exit_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "process should have exited"
        );
    }

    #[test]
    fn real_spawner_kill_terminates_the_process() {
        let spawner = RealPtySpawner;
        let mut env = HashMap::new();
        env.insert(
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_default(),
        );

        let (exit_tx, exit_rx) = mpsc::channel::<()>();
        let proc = spawner
            .spawn(
                "/bin/sh",
                &std::env::temp_dir(),
                80,
                24,
                env,
                Box::new(|_| {}),
                Box::new(move || {
                    let _ = exit_tx.send(());
                }),
            )
            .expect("spawn should succeed");

        proc.kill();
        assert!(
            exit_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "kill should trigger on_exit"
        );
    }

    #[test]
    fn real_shell_query_can_back_a_login_path_resolver() {
        // Sanity check the two real pieces (RealPtySpawner's env plumbing,
        // RealShellQuery's login resolution) compose without a dedicated
        // integration test needing its own OS pty.
        let resolver = LoginPathResolver::new(RealShellQuery, cfg!(target_os = "windows"));
        let path = resolver.login_path(&std::env::var("PATH").unwrap_or_default(), "/bin/sh");
        assert!(!path.is_empty());
    }
}
