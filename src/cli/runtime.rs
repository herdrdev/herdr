use crate::api::schema::{
    EmptyParams, Method, PaneFocusDirectionParams, PaneInputSetParams, PaneMoveParams,
    PaneRenameParams, PaneResizeParams, PaneSplitParams, PaneSwapParams, PaneTarget,
    PaneZoomParams, Request, TabCreateParams, TabListParams, TabRenameParams, TabTarget,
    WorkspaceCreateParams, WorkspaceRenameParams, WorkspaceTarget, WorktreeCreateParams,
    WorktreeListParams, WorktreeOpenParams, WorktreeRemoveParams,
};
use crate::task::TaskStatus;

fn print_method_response(id: &'static str, method: Method) -> std::io::Result<i32> {
    super::print_response(&super::send_request(&Request {
        id: id.into(),
        method,
    })?)
}

pub(super) fn workspace_list() -> std::io::Result<i32> {
    print_method_response(
        "cli:workspace:list",
        Method::WorkspaceList(EmptyParams::default()),
    )
}

pub(super) fn task_list(status: Option<TaskStatus>) -> std::io::Result<i32> {
    print_method_response(
        "cli:task:list",
        Method::TaskList(crate::api::schema::TaskListParams { status }),
    )
}

pub(super) fn task_board() -> std::io::Result<i32> {
    let response = super::send_request(&Request {
        id: "cli:task:board".into(),
        method: Method::TaskList(crate::api::schema::TaskListParams { status: None }),
    })?;
    if response.get("error").is_some() {
        return super::print_response(&response);
    }
    let tasks = response
        .get("result")
        .and_then(|result| result.get("tasks"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    for status in [
        TaskStatus::Backlog,
        TaskStatus::Ready,
        TaskStatus::Running,
        TaskStatus::Blocked,
        TaskStatus::Review,
        TaskStatus::Done,
        TaskStatus::Failed,
        TaskStatus::Cancelled,
    ] {
        println!("\n{:?}", status);
        for task in tasks.iter().filter(|task| {
            serde_json::from_value::<TaskStatus>(task.get("status").cloned().unwrap_or_default())
                .ok()
                == Some(status)
        }) {
            let id = task
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?");
            let title = task
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("(untitled)");
            let priority = task
                .get("priority")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(100);
            println!("  {id:<10} p{priority:<3} {title}");
        }
    }
    Ok(0)
}

pub(super) fn task_get(task_id: String) -> std::io::Result<i32> {
    print_method_response(
        "cli:task:get",
        Method::TaskGet(crate::api::schema::TaskTarget { task_id }),
    )
}

pub(super) fn task_create(params: crate::api::schema::TaskCreateParams) -> std::io::Result<i32> {
    print_method_response("cli:task:create", Method::TaskCreate(params))
}

pub(super) fn task_update(params: crate::api::schema::TaskUpdateParams) -> std::io::Result<i32> {
    print_method_response("cli:task:update", Method::TaskUpdate(params))
}

pub(super) fn task_attach(params: crate::api::schema::TaskAttachParams) -> std::io::Result<i32> {
    print_method_response("cli:task:attach", Method::TaskAttach(params))
}

pub(super) fn task_dispatch(
    params: crate::api::schema::TaskDispatchParams,
) -> std::io::Result<i32> {
    print_method_response("cli:task:dispatch", Method::TaskDispatch(params))
}

pub(super) fn task_report(params: crate::api::schema::TaskReportParams) -> std::io::Result<i32> {
    print_method_response("cli:task:report", Method::TaskReport(params))
}

pub(super) fn workspace_create(params: WorkspaceCreateParams) -> std::io::Result<i32> {
    print_method_response("cli:workspace:create", Method::WorkspaceCreate(params))
}

pub(super) fn workspace_get(workspace_id: String) -> std::io::Result<i32> {
    print_method_response(
        "cli:workspace:get",
        Method::WorkspaceGet(WorkspaceTarget { workspace_id }),
    )
}

pub(super) fn workspace_focus(workspace_id: String) -> std::io::Result<i32> {
    print_method_response(
        "cli:workspace:focus",
        Method::WorkspaceFocus(WorkspaceTarget { workspace_id }),
    )
}

pub(super) fn workspace_rename(params: WorkspaceRenameParams) -> std::io::Result<i32> {
    print_method_response("cli:workspace:rename", Method::WorkspaceRename(params))
}

pub(super) fn workspace_close(workspace_id: String) -> std::io::Result<i32> {
    print_method_response(
        "cli:workspace:close",
        Method::WorkspaceClose(WorkspaceTarget { workspace_id }),
    )
}

pub(super) fn tab_list(params: TabListParams) -> std::io::Result<i32> {
    print_method_response("cli:tab:list", Method::TabList(params))
}

pub(super) fn tab_create(params: TabCreateParams) -> std::io::Result<i32> {
    print_method_response("cli:tab:create", Method::TabCreate(params))
}

pub(super) fn tab_get(tab_id: String) -> std::io::Result<i32> {
    print_method_response("cli:tab:get", Method::TabGet(TabTarget { tab_id }))
}

pub(super) fn tab_focus(tab_id: String) -> std::io::Result<i32> {
    print_method_response("cli:tab:focus", Method::TabFocus(TabTarget { tab_id }))
}

pub(super) fn tab_rename(params: TabRenameParams) -> std::io::Result<i32> {
    print_method_response("cli:tab:rename", Method::TabRename(params))
}

pub(super) fn tab_close(tab_id: String) -> std::io::Result<i32> {
    print_method_response("cli:tab:close", Method::TabClose(TabTarget { tab_id }))
}

pub(super) fn worktree_list(params: WorktreeListParams) -> std::io::Result<i32> {
    print_method_response("cli:worktree:list", Method::WorktreeList(params))
}

pub(super) fn worktree_create(params: WorktreeCreateParams) -> std::io::Result<i32> {
    print_method_response("cli:worktree:create", Method::WorktreeCreate(params))
}

pub(super) fn worktree_open(params: WorktreeOpenParams) -> std::io::Result<i32> {
    print_method_response("cli:worktree:open", Method::WorktreeOpen(params))
}

pub(super) fn worktree_remove(params: WorktreeRemoveParams) -> std::io::Result<i32> {
    print_method_response("cli:worktree:remove", Method::WorktreeRemove(params))
}

pub(super) fn pane_focus(params: PaneFocusDirectionParams) -> std::io::Result<i32> {
    print_method_response("cli:pane:focus", Method::PaneFocusDirection(params))
}

pub(super) fn pane_resize(params: PaneResizeParams) -> std::io::Result<i32> {
    print_method_response("cli:pane:resize", Method::PaneResize(params))
}

pub(super) fn pane_zoom(params: PaneZoomParams) -> std::io::Result<i32> {
    print_method_response("cli:pane:zoom", Method::PaneZoom(params))
}

pub(super) fn pane_rename(params: PaneRenameParams) -> std::io::Result<i32> {
    print_method_response("cli:pane:rename", Method::PaneRename(params))
}

pub(super) fn pane_input_set(params: PaneInputSetParams) -> std::io::Result<i32> {
    print_method_response("cli:pane:input:set", Method::PaneInputSet(params))
}

pub(super) fn pane_split(params: PaneSplitParams) -> std::io::Result<i32> {
    print_method_response("cli:pane:split", Method::PaneSplit(params))
}

pub(super) fn pane_swap(params: PaneSwapParams) -> std::io::Result<i32> {
    print_method_response("cli:pane:swap", Method::PaneSwap(params))
}

pub(super) fn pane_move(params: PaneMoveParams) -> std::io::Result<i32> {
    print_method_response("cli:pane:move", Method::PaneMove(params))
}

pub(super) fn pane_close(pane_id: String) -> std::io::Result<i32> {
    print_method_response("cli:pane:close", Method::PaneClose(PaneTarget { pane_id }))
}
