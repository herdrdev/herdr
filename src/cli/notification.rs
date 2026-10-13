#[path = "notification_bindings.rs"]
mod bindings;

pub(crate) use bindings::command;

pub(super) fn run_notification_command(args: &[String]) -> std::io::Result<i32> {
    super::run_json_request(bindings::request(args))
}
