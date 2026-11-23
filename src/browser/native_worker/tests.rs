use super::*;
use eframe::egui::{self, Modifiers};
use std::sync::atomic::AtomicUsize;

#[test]
fn repaint_deadlines_coalesce_and_immediate_requests_are_rate_limited() {
    let (sender, incoming) = mpsc::sync_channel(1);
    let schedule = Schedule {
        due: Mutex::new(None),
        sender,
    };
    let now = Instant::now();
    schedule.request(Duration::from_secs(10));
    let later = schedule.deadline(now).unwrap();
    schedule.request(Duration::from_secs(20));
    assert_eq!(schedule.deadline(now), Some(later));
    // A full wake queue must still retain the earlier deadline.
    schedule.request(Duration::ZERO);
    assert_eq!(schedule.deadline(now), Some(now + FRAME_INTERVAL));
    assert!(!schedule.take_due(now, now));
    assert!(schedule.take_due(now + FRAME_INTERVAL, now));
    assert!(schedule.deadline(now).is_none());
    assert_eq!(incoming.try_iter().count(), 1);
    schedule.request(Duration::MAX);
    assert!(schedule.deadline(now).is_none());
}

fn input(epoch: u64, events: Vec<egui::Event>) -> Input {
    Input {
        size: [1200.0, 800.0],
        pixels_per_point: 1.0,
        focused: false,
        events,
        epoch: Some(epoch),
    }
}

fn frame(worker: &Worker) -> Box<Frame> {
    match worker
        .incoming
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
    {
        Event::Frame(frame) => frame,
        Event::Failed(error) => panic!("{error}"),
        _ => panic!("Expected a frame"),
    }
}

fn acknowledge(worker: &Worker, reset: bool) {
    worker.sender.send(Command::Applied { reset }).unwrap();
}

fn close_result(worker: &Worker, action: CloseAction) -> CloseResult {
    worker.sender.send(Command::Close(action)).unwrap();
    loop {
        match worker
            .incoming
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
        {
            Event::Close(result) => return result,
            Event::Frame(_) => acknowledge(worker, false),
            Event::Failed(error) => panic!("{error}"),
            _ => panic!("Unexpected session event"),
        }
    }
}

fn replace_text(worker: &Worker, epoch: u64, text: &str) {
    // Source begins below the toolbar, tabs, and breadcrumb row.
    let metrics = crate::ui_theme::nord_light().metrics;
    let position = egui::pos2(
        600.0,
        metrics.toolbar_height + metrics.tab_height + 28.0 + metrics.editor_top_padding + 10.0,
    );
    let mut next = input(
        epoch,
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
            egui::Event::Key {
                key: egui::Key::A,
                physical_key: Some(egui::Key::A),
                pressed: true,
                repeat: false,
                modifiers: Modifiers {
                    ctrl: true,
                    command: true,
                    ..Modifiers::NONE
                },
            },
            egui::Event::Key {
                key: egui::Key::A,
                physical_key: Some(egui::Key::A),
                pressed: false,
                repeat: false,
                modifiers: Modifiers {
                    ctrl: true,
                    command: true,
                    ..Modifiers::NONE
                },
            },
            egui::Event::Text(text.into()),
        ],
    );
    next.focused = true;
    worker.sender.send(Command::Input(next)).unwrap();
}

#[test]
fn native_session_idle_delivery_backpressure_input_close_and_cleanup() {
    const CHILD: &str = "FOCUS_NATIVE_TEST_ROOT";
    let Some(root) = std::env::var_os(CHILD).map(PathBuf::from) else {
        let root = tempfile::tempdir().unwrap();
        let shell = root.path().join("quiet-shell");
        std::fs::write(&shell, "#!/bin/sh\nprintf '%s' \"$$\" > \"$FOCUS_NATIVE_TEST_ROOT/shell-pid\"\nexec sleep 60\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o700)).unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "browser::native_worker::tests::native_session_idle_delivery_backpressure_input_close_and_cleanup", "--nocapture"])
            .env(CHILD, root.path())
            .env("XDG_DATA_HOME", root.path().join("data"))
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("SHELL", shell)
            .output().unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    };
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let source = workspace.join("example.txt");
    std::fs::write(&source, "original").unwrap();
    let (_, settings_path) = crate::settings::load_workspace(Some(&workspace)).unwrap();
    crate::settings::write_workspace(
        &settings_path,
        &crate::settings::Settings {
            workspace: Some(workspace.clone()),
            session_workspace: Some(workspace.clone()),
            open_files: vec![source.clone()],
            active_file: Some(source.clone()),
            ..Default::default()
        },
    )
    .unwrap();

    let args = Arguments {
        workspace: Some(workspace.clone()),
        ..Default::default()
    };
    let notifications = Arc::new(AtomicUsize::new(0));
    let count = notifications.clone();
    let worker = Worker::start(&args, move || {
        count.fetch_add(1, Ordering::SeqCst);
    })
    .unwrap();
    let epoch = worker.control.epoch;
    assert!(worker.control.owned);
    worker
        .sender
        .send(Command::Input(input(epoch, vec![])))
        .unwrap();
    let initial = frame(&worker);
    assert!(initial.reset && !initial.textures.is_empty());
    acknowledge(&worker, false);

    // Allow initial layout and font uploads to settle, consuming each snapshot.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            Instant::now() < deadline,
            "Idle native session never settled"
        );
        match worker.incoming.recv_timeout(Duration::from_millis(250)) {
            Ok(Event::Frame(_)) => acknowledge(&worker, false),
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            _ => panic!("Unexpected idle event"),
        }
    }
    let before = notifications.load(Ordering::SeqCst);
    for _ in 0..40 {
        // Even forced backend renders with advancing time must not present identical frames.
        worker
            .sender
            .send(Command::Input(input(epoch, vec![])))
            .unwrap();
    }
    assert!(matches!(
        worker.incoming.recv_timeout(Duration::from_millis(350)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    assert_eq!(notifications.load(Ordering::SeqCst), before);

    let mut resized = input(epoch, vec![]);
    resized.size = [1100.0, 800.0];
    worker.sender.send(Command::Input(resized)).unwrap();
    let resized = frame(&worker);
    assert_eq!(resized.size, [1100.0, 800.0]);
    // Hold the frame unacknowledged. Backend input, saving and HTTP still work.
    replace_text(&worker, epoch, "native queued edit");
    assert_eq!(
        close_result(&worker, CloseAction::Check),
        CloseResult::Dirty
    );
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "original");
    assert!(worker.incoming.try_recv().is_err());
    assert_eq!(
        close_result(&worker, CloseAction::Save),
        CloseResult::Allowed
    );
    assert_eq!(
        std::fs::read_to_string(&source).unwrap(),
        "native queued edit"
    );

    replace_text(&worker, epoch, "second edit");
    assert_eq!(
        close_result(&worker, CloseAction::Check),
        CloseResult::Dirty
    );
    let backup = root.join("backup.txt");
    std::fs::rename(&source, &backup).unwrap();
    std::fs::create_dir(&source).unwrap();
    assert_eq!(
        close_result(&worker, CloseAction::Save),
        CloseResult::SaveFailed
    );
    assert_eq!(
        std::fs::read_to_string(&backup).unwrap(),
        "native queued edit"
    );
    assert_eq!(
        close_result(&worker, CloseAction::Discard),
        CloseResult::Allowed
    );
    std::fs::remove_dir(&source).unwrap();
    std::fs::rename(&backup, &source).unwrap();
    assert_eq!(
        close_result(&worker, CloseAction::Save),
        CloseResult::Allowed
    );
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "second edit");

    let mut copy = input(
        epoch,
        vec![
            egui::Event::Key {
                key: egui::Key::A,
                physical_key: Some(egui::Key::A),
                pressed: true,
                repeat: false,
                modifiers: Modifiers {
                    ctrl: true,
                    command: true,
                    ..Modifiers::NONE
                },
            },
            egui::Event::Copy,
            egui::Event::Key {
                key: egui::Key::A,
                physical_key: Some(egui::Key::A),
                pressed: false,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
        ],
    );
    copy.focused = true;
    worker.sender.send(Command::Input(copy)).unwrap();
    assert_eq!(
        close_result(&worker, CloseAction::Check),
        CloseResult::Allowed
    );
    acknowledge(&worker, false);
    let copied = frame(&worker);
    assert_eq!(
        copied.output.commands,
        [egui::OutputCommand::CopyText("second edit".into())]
    );

    let record = registry::records().unwrap().pop().unwrap();
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let post = |path: &str| {
        client
            .post(format!("{}/api/{path}", record.endpoint))
            .header("Origin", &record.endpoint)
            .header("X-Focus-Token", &record.token)
            .header("X-Focus-Client", "remote-test")
            .header("X-Focus-Remote", "1")
    };
    assert!(
        !post("connect")
            .send()
            .unwrap()
            .json::<Control>()
            .unwrap()
            .owned
    );
    let remote = post("claim").send().unwrap().json::<Control>().unwrap();
    assert!(remote.owned && remote.epoch > epoch);
    acknowledge(&worker, true);
    let reset = frame(&worker);
    assert!(reset.reset && !reset.textures.is_empty());
    assert!(!reset.control.owned);
    assert_eq!(reset.control.epoch, remote.epoch);
    assert!(
        reset.output.commands.is_empty(),
        "Clipboard command was replayed"
    );
    acknowledge(&worker, false);
    worker.sender.send(Command::Claim).unwrap();
    let reclaimed = frame(&worker);
    assert!(reclaimed.control.owned && reclaimed.control.epoch > remote.epoch);

    let pid: i32 = std::fs::read_to_string(root.join("shell-pid"))
        .unwrap()
        .parse()
        .unwrap();
    let start = Instant::now();
    drop(worker); // No acknowledgment, and no GUI thread, are needed to stop.
    assert!(start.elapsed() < Duration::from_secs(3));
    assert!(registry::records().unwrap().is_empty());
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "Session PTY child was not reaped"
    );
    assert!(settings_path.is_file());

    let invalid = Arguments {
        headless: true,
        workspace: Some(root.join("missing")),
        ..Default::default()
    };
    assert!(Worker::start(&invalid, || {}).is_err());
    assert!(registry::records().unwrap().is_empty());
}
