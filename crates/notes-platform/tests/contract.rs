//! Platform contract. The Linux expectations are the pre-existing engine
//! behaviour; the macOS expectations document the port.
use notes_platform::{dirs, net, open, process, tools, url_scheme};
use std::path::{Path, PathBuf};
use std::time::Duration;

// Environment is process-global: all env-dependent checks live in one test.
#[test]
fn directories_follow_xdg_overrides_and_platform_defaults() {
    let home = Path::new("/srv/example-home");
    let saved: Vec<_> = ["XDG_STATE_HOME", "XDG_DATA_HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME"].iter().map(|k| (*k, std::env::var_os(k))).collect();
    for (k, _) in &saved {
        std::env::remove_var(k);
    }
    if cfg!(target_os = "macos") {
        let base = PathBuf::from("/srv/example-home/Library/Application Support/org.linuxnotes.desktop");
        assert_eq!(dirs::state_home(home), base);
        assert_eq!(dirs::data_home(home), base);
        assert_eq!(dirs::config_home(home), base);
        assert_eq!(dirs::library_dir(home), base.join("samsung-notes-linux"));
        assert_eq!(dirs::research_dir(home), base.join("samsung-notes-research"));
    } else {
        assert_eq!(dirs::state_home(home), PathBuf::from("/srv/example-home/.local/state"));
        assert_eq!(dirs::data_home(home), PathBuf::from("/srv/example-home/.local/share"));
        assert_eq!(dirs::config_home(home), PathBuf::from("/srv/example-home/.config"));
        assert_eq!(dirs::library_dir(home), PathBuf::from("/srv/example-home/.local/state/samsung-notes-linux"));
        assert_eq!(dirs::research_dir(home), PathBuf::from("/srv/example-home/.local/state/samsung-notes-research"));
    }
    // An explicit XDG variable wins on every platform (isolated profiles).
    std::env::set_var("XDG_STATE_HOME", "/isolated/state");
    std::env::set_var("XDG_DATA_HOME", "/isolated/data");
    std::env::set_var("XDG_CONFIG_HOME", "/isolated/config");
    assert_eq!(dirs::library_dir(home), PathBuf::from("/isolated/state/samsung-notes-linux"));
    assert_eq!(dirs::research_dir(home), PathBuf::from("/isolated/state/samsung-notes-research"));
    assert_eq!(dirs::data_home(home), PathBuf::from("/isolated/data"));
    assert_eq!(dirs::config_home(home), PathBuf::from("/isolated/config"));
    // XDG spec: empty or relative values are ignored (tools/_xdg.py too).
    let default_state = if cfg!(target_os = "macos") {
        PathBuf::from("/srv/example-home/Library/Application Support/org.linuxnotes.desktop")
    } else {
        PathBuf::from("/srv/example-home/.local/state")
    };
    std::env::set_var("XDG_STATE_HOME", "");
    assert_eq!(dirs::state_home(home), default_state);
    std::env::set_var("XDG_STATE_HOME", "relative/state");
    assert_eq!(dirs::state_home(home), default_state);
    // Runtime dir: $XDG_RUNTIME_DIR/samsung-notes-linux, private, created on demand.
    let saved_runtime = std::env::var_os("XDG_RUNTIME_DIR");
    let scratch = std::env::temp_dir().join(format!("notes-platform-rt-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    std::env::set_var("XDG_RUNTIME_DIR", &scratch);
    if cfg!(target_os = "macos") {
        assert_eq!(dirs::runtime_dir(), None);
    } else {
        use std::os::unix::fs::PermissionsExt;
        let dir = dirs::runtime_dir().unwrap();
        assert_eq!(dir, scratch.join("samsung-notes-linux"));
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        std::env::set_var("XDG_RUNTIME_DIR", "relative");
        assert_eq!(dirs::runtime_dir(), None);
    }
    std::env::remove_var("XDG_RUNTIME_DIR");
    assert_eq!(dirs::runtime_dir(), None);
    match saved_runtime {
        Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
        None => std::env::remove_var("XDG_RUNTIME_DIR"),
    }
    let _ = std::fs::remove_dir_all(&scratch);
    std::env::remove_var("XDG_CACHE_HOME");
    if !cfg!(target_os = "macos") {
        assert_eq!(dirs::cache_home(home), PathBuf::from("/srv/example-home/.cache"));
    }
    for (k, v) in saved {
        match v {
            Some(v) => std::env::set_var(k, v),
            None => std::env::remove_var(k),
        }
    }
}

#[test]
fn downloads_falls_back_to_home() {
    let scratch = std::env::temp_dir().join(format!("notes-platform-dl-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let got = dirs::downloads_dir(&scratch);
    assert!(got == scratch || got.is_dir(), "{got:?}");
    if cfg!(target_os = "macos") {
        std::fs::create_dir_all(scratch.join("Downloads")).unwrap();
        assert_eq!(dirs::downloads_dir(&scratch), scratch.join("Downloads"));
    }
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn opener_and_scheme_registration() {
    if cfg!(target_os = "macos") {
        assert_eq!(open::OPENER, "open");
        assert_eq!(url_scheme::REGISTRATION, url_scheme::Registration::AppBundle);
    } else {
        assert_eq!(open::OPENER, "xdg-open");
        assert_eq!(url_scheme::REGISTRATION, url_scheme::Registration::XdgDesktopEntry);
    }
    assert_eq!(open::command().get_program(), open::OPENER);
    assert_eq!(url_scheme::SCHEME, "ms-app");
}

#[test]
fn tool_programs() {
    use tools::Tool;
    if cfg!(target_os = "macos") {
        assert!(tools::locate(Tool::Bwrap).is_none());
        assert!(!Tool::Bwrap.used_here());
        assert_eq!(tools::install_hint(Tool::Pdftoppm).as_deref(), Some("brew install poppler"));
        assert_eq!(tools::with_install_hint("x", Tool::Magick), "x (brew install imagemagick)");
        for tool in [Tool::Pdftoppm, Tool::RsvgConvert, Tool::Magick] {
            if let Some(found) = tools::locate(tool) {
                assert_eq!(tools::program(tool), found);
            } else {
                assert_eq!(tools::program(tool), PathBuf::from(tool.name()));
            }
        }
    } else {
        // Fixed sandbox paths and a PATH-resolved ImageMagick, as before.
        assert_eq!(tools::program(Tool::Pdftoppm), PathBuf::from("/usr/bin/pdftoppm"));
        assert_eq!(tools::program(Tool::RsvgConvert), PathBuf::from("/usr/bin/rsvg-convert"));
        assert_eq!(tools::program(Tool::Bwrap), PathBuf::from("/usr/bin/bwrap"));
        assert_eq!(tools::program(Tool::Magick), PathBuf::from("magick"));
        assert_eq!(tools::available(Tool::Pdftoppm), Path::new("/usr/bin/pdftoppm").is_file());
        assert_eq!(tools::available(Tool::Bwrap), Path::new("/usr/bin/bwrap").is_file());
        assert_eq!(tools::install_hint(Tool::Magick), None);
        assert_eq!(tools::with_install_hint("PDF oluşturmak için ImageMagick gerekli.", Tool::Magick), "PDF oluşturmak için ImageMagick gerekli.");
    }
    for line in tools::missing_report() {
        assert!(line.contains("not found"));
    }
}

#[test]
fn capture_runs_and_times_out() {
    let (code, out) = process::capture(&["sh", "-c", "printf ok"], Duration::from_secs(5)).unwrap();
    assert_eq!((code, out.as_slice()), (0, b"ok".as_slice()));
    assert!(process::capture(&["sh", "-c", "sleep 5"], Duration::from_millis(50)).is_none());
    assert!(process::capture(&["/nonexistent/notes-platform-tool"], Duration::from_secs(1)).is_none());
}

#[test]
fn limits_and_parent_binding_in_a_child() {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new("sh");
    command.args(["-c", "ulimit -t"]).stdout(std::process::Stdio::piped());
    unsafe {
        command.pre_exec(|| {
            process::set_parent_death_signal()?;
            process::set_limit(process::Limit::Cpu, 7)?;
            process::set_limit(process::Limit::AddressSpace, 1 << 32)?;
            process::set_limit(process::Limit::Core, 0)?;
            process::pin_lowest_cpu()?;
            Ok(())
        });
    }
    let out = command.output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "7");
}

#[test]
fn loopback_listener_without_reuseaddr() {
    let listener = net::bind_tcp_v4("127.0.0.1:0".parse().unwrap(), 5).unwrap();
    let port = listener.local_addr().unwrap().port();
    assert!(port > 0);
    let _client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    let (_server_side, _) = listener.accept().unwrap();
    // Second bind on the same port fails (no SO_REUSEADDR, still listening).
    assert!(net::bind_tcp_v4(format!("127.0.0.1:{port}").parse().unwrap(), 5).is_err());
    use std::os::unix::io::AsRawFd;
    let flags = unsafe { libc::fcntl(listener.as_raw_fd(), libc::F_GETFD) };
    assert!(flags & libc::FD_CLOEXEC != 0);
}

#[test]
fn macos_isolation_prefix_shape() {
    use notes_platform::isolation;
    assert_eq!(isolation::sbpl_string("/a \"b\"\\c"), "\"/a \\\"b\\\"\\\\c\"");
    let profile = isolation::seatbelt_profile(Some(Path::new("/private/var/folders/x/T/notes-pdf-1")));
    assert!(profile.starts_with("(version 1)\n(allow default)\n(deny network*)\n(deny file-write*)\n"));
    assert!(profile.contains("(subpath \"/private/var/folders/x/T/notes-pdf-1\")"));
    assert!(!isolation::seatbelt_profile(None).contains("subpath"));
    let args = isolation::macos_prefix(None, Path::new("/var/empty"), true);
    assert_eq!(&args[..5], ["/usr/bin/env", "-i", "HOME=/var/empty", "PATH=/usr/bin:/bin", "LC_ALL=C"]);
    assert_eq!(&args[5..7], ["/usr/bin/sandbox-exec", "-p"]);
    assert_eq!(isolation::macos_prefix(None, Path::new("/h"), false).len(), 5);
    if !cfg!(target_os = "macos") {
        assert!(!isolation::seatbelt_usable());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn macos_seatbelt_blocks_writes_outside_the_output_dir() {
    use notes_platform::isolation;
    if !isolation::seatbelt_usable() {
        eprintln!("sandbox-exec unavailable; Seatbelt layer skipped");
        return;
    }
    let out = std::env::temp_dir().join(format!("notes-platform-sb-{}", std::process::id()));
    std::fs::create_dir_all(&out).unwrap();
    let outside = std::env::temp_dir().join(format!("notes-platform-sb-outside-{}", std::process::id()));
    let mut argv = isolation::prefix(Some(&out), &out);
    argv.extend(["/bin/sh".into(), "-c".into(), format!("echo ok > '{}/inside' && ! echo no > '{}'", out.display(), outside.display())]);
    let status = std::process::Command::new(&argv[0]).args(&argv[1..]).status().unwrap();
    assert!(status.success());
    assert!(out.join("inside").is_file());
    assert!(!outside.exists());
    let _ = std::fs::remove_dir_all(&out);
}
