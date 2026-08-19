#![cfg(unix)]

#[test]
fn constructs_watcher_options() {
    let options = herdr_agent_watcher::daemon::DaemonOptions::new("/tmp/vimeflow-watcher-smoke");
    assert_eq!(
        options.state_dir,
        std::path::Path::new("/tmp/vimeflow-watcher-smoke")
    );
}
