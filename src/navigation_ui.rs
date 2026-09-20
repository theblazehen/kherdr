//! Stock navigation projection and action routing, separate from terminal paints.
use crate::{
    App,
    client::Pane,
    ui::{AppWindow, MachineRow, SidebarRow, TabView},
};
use slint::{Model, ModelRc, VecModel};
use std::collections::BTreeMap;

impl App {
    pub(crate) fn navigation_model(&self, ui: &AppWindow) {
        let selected = |a: &Pane| {
            self.endpoints
                .active
                .focus
                .as_ref()
                .is_some_and(|f| f.pane_id.as_deref() == Some(a.pane_id.as_str()))
        };
        let active = self.endpoints.active.panes.iter().find(|p| selected(p));
        let focused_workspace = self
            .endpoints
            .active
            .focused_workspace
            .as_deref()
            .or_else(|| {
                self.endpoints
                    .active
                    .workspaces
                    .iter()
                    .find(|w| w.focused)
                    .map(|w| w.workspace_id.as_str())
            })
            .or_else(|| active.map(|p| p.workspace_id.as_str()));
        let tabs: Vec<TabView> = self
            .endpoints
            .active
            .tabs
            .iter()
            .filter(|t| Some(t.workspace_id.as_str()) == focused_workspace)
            .map(|t| TabView {
                tab_id: t.tab_id.clone().into(),
                name: t.name.clone().into(),
                status: t.status.clone().into(),
                selected: t.focused || active.is_some_and(|p| p.tab_id == t.tab_id),
            })
            .collect();
        let mut tab_model = ui.get_tabs();
        if update_navigation_entries(&mut tab_model, tabs) {
            ui.set_tabs(tab_model);
        }
        let mut tab_counts = BTreeMap::<&str, usize>::new();
        let zoomed: BTreeMap<&str, &str> = self.endpoints.active.tabs.iter()
            .filter(|tab| tab.zoomed)
            .map(|tab| (tab.tab_id.as_str(), tab.pane_id.as_str())).collect();
        for pane in &self.endpoints.active.panes {
            *tab_counts.entry(pane.tab_id.as_str()).or_default() += 1;
        }
        // Selection never reorders machines. Collapse and agent ordering are
        // independent presentation choices, not endpoint lifecycle operations.
        let mut endpoints: Vec<_> = self.endpoints.iter().collect();
        endpoints.sort_unstable_by_key(|session| session.id);
        let mut machines = Vec::new();
        let mut agents = Vec::new();
        let mut blocked = 0;
        let mut unknown = false;
        for session in endpoints {
            let endpoint_id: slint::SharedString = session.id.to_string().into();
            let profile = session.profile.as_ref();
            let host = profile.map_or("This Kindle", |profile| profile.name.as_str());
            let name = profile.map_or("Local", |profile| profile.herdr_session.as_str());
            let fresh = session.connected && session.navigation_ready;
            unknown |= !fresh;
            let expanded = self.expanded_machines.contains(&session.id);
            machines.push(MachineRow {
                endpoint_id: endpoint_id.clone(), machine: true, local: profile.is_none(), expanded, enabled: true,
                entry: SidebarRow { label: format!("{host} · {name}").into(),
                    detail: if fresh { "" } else if session.connecting { "Connecting…" } else { "Unavailable" }.into(),
                    ..Default::default() },
            });
            if expanded {
                for workspace in &session.workspaces {
                    let group_collapsed = self.collapsed_groups.get(&session.id)
                        .is_some_and(|groups| groups.contains(&workspace.worktree_key));
                    if workspace.worktree_linked && group_collapsed { continue; }
                    let entry = SidebarRow {
                        endpoint_id: endpoint_id.clone(),
                        label: workspace.label.clone().into(), resource_name: workspace.label.clone().into(),
                        detail: workspace.worktree_label.clone().into(),
                        status: if fresh { workspace.status.as_str() } else { "unknown" }.into(),
                        workspace_id: workspace.workspace_id.clone().into(),
                        worktree_linked: workspace.worktree_linked, worktree_key: workspace.worktree_key.clone().into(),
                        has_worktree_children: !workspace.worktree_linked && !workspace.worktree_key.is_empty()
                            && session.workspaces.iter().any(|child| child.worktree_linked && child.worktree_key == workspace.worktree_key),
                        group_collapsed,
                        selected: session.id == self.endpoints.active.id && Some(workspace.workspace_id.as_str()) == focused_workspace,
                        ..Default::default()
                    };
                    machines.push(MachineRow { endpoint_id: endpoint_id.clone(), machine: false,
                        local: profile.is_none(), expanded: false, enabled: fresh, entry });
                }
            }
            // These are only ClientShellSnapshot.agents. Workspace/tab status
            // rollups and ordinary shells must never inflate the badge.
            let group_start = agents.len();
            for agent in &session.agents {
                let status = if fresh { agent.status.as_str() } else { "unknown" };
                blocked += i32::from(status == "blocked");
                unknown |= !matches!(status, "blocked" | "working" | "done" | "idle");
                let workspace = session.workspaces.iter().find(|w| w.workspace_id == agent.workspace_id)
                    .map_or(agent.workspace_id.as_str(), |w| w.label.as_str());
                let label = match agent.agent.as_str() { "omp" => "OMP", "codex" => "Codex", "" => "Agent", other => other };
                agents.push(SidebarRow {
                    endpoint_id: endpoint_id.clone(), pane_id: agent.pane_id.clone().into(),
                    available: fresh && session.panes.iter().any(|pane| pane.pane_id == agent.pane_id),
                    workspace_id: agent.workspace_id.clone().into(), label: label.into(), resource_name: agent.name.clone().into(),
                    detail: format!("{host} / {workspace} / {}", agent.name).into(), status: status.into(),
                    selected: session.id == self.endpoints.active.id && session.focus.as_ref()
                        .is_some_and(|focus| focus.pane_id.as_deref() == Some(agent.pane_id.as_str())),
                    ..Default::default()
                });
            }
            // Keep workspace groups together even when the server's agent list
            // interleaves them. Priority changes only this separate agent list.
            agents[group_start..].sort_by_cached_key(|agent| session.workspaces.iter()
                .position(|workspace| workspace.workspace_id == agent.workspace_id.as_str())
                .unwrap_or(usize::MAX));
        }
        if ui.get_agents_priority() {
            agents.sort_by_key(|agent| match agent.status.as_str() { "blocked" => 0, "unknown" | "" => 1, "working" => 2, "done" => 3, _ => 4 });
        }
        let mut machine_model = ui.get_machine_rows();
        if update_navigation_entries(&mut machine_model, machines) { ui.set_machine_rows(machine_model); }
        let mut agent_model = ui.get_agent_rows();
        if update_navigation_entries(&mut agent_model, agents) { ui.set_agent_rows(agent_model); }
        ui.set_attention_count(blocked);
        ui.set_attention_unknown(unknown);
        let pane_rows: Vec<SidebarRow> = self
            .endpoints
            .active
            .panes
            .iter()
            .filter(|pane| Some(pane.workspace_id.as_str()) == focused_workspace)
            .map(|pane| SidebarRow {
                label: pane.name.clone().into(),
                resource_name: pane.name.clone().into(),
                detail: pane.detail.clone().into(),
                status: pane.status.clone().into(),
                pane_id: pane.pane_id.clone().into(),
                workspace_id: pane.workspace_id.clone().into(),
                can_zoom: tab_counts.get(pane.tab_id.as_str()).copied().unwrap_or(0) > 1,
                zoomed: zoomed.get(pane.tab_id.as_str()).copied() == Some(pane.pane_id.as_str()),
                can_swap: active.is_some_and(|focused| focused.pane_id != pane.pane_id),
                custom_label: pane.custom_label,
                selected: selected(pane),
                ..Default::default()
            })
            .collect();
        ui.set_focused_pane(pane_rows.iter().find(|pane| pane.selected).cloned().unwrap_or_default());
        let mut pane_model = ui.get_pane_rows();
        if update_navigation_entries(&mut pane_model, pane_rows) {
            ui.set_pane_rows(pane_model);
        }
        ui.set_selected_name(active.map_or("", |p| p.name.as_str()).into());
        ui.set_focused_pane_id(active.map_or("", |p| p.pane_id.as_str()).into());
        ui.set_focused_workspace_id(focused_workspace.unwrap_or("").into());
        ui.set_selected_workspace_label(self.endpoints.active.workspaces.iter()
            .find(|workspace| Some(workspace.workspace_id.as_str()) == focused_workspace)
            .map(|workspace| workspace.label.clone())
            .unwrap_or_default().into());
    }

    pub(crate) fn stock_action(
        &mut self,
        ui: &AppWindow,
        action: &str,
        target: &str,
        workspace: &str,
        value: &str,
    ) {
        if action == "toggle_group" {
            let groups = self.collapsed_groups.entry(self.endpoints.active.id).or_default();
            if !target.is_empty() && !groups.remove(target) {
                groups.insert(target.to_owned());
            }
            self.navigation_model(ui);
            return;
        }
        let result = (|| -> Result<(), String> {
            let client = self
                .endpoints
                .active
                .client
                .as_ref()
                .ok_or("Not connected")?;
            match action {
                "new_tab" => {
                    if workspace.is_empty() {
                        client.new_tab()
                    } else {
                        client.new_tab_in(workspace)
                    }
                }
                "rename_workspace" => client.rename_workspace(target, value),
                "close_workspace" => client.close_workspace(target),
                "new_worktree" => client.new_worktree(target, value),
                "open_worktree" => client.open_worktree(target, value),
                "remove_worktree" => client.remove_worktree(target),
                "rename_tab" => client.rename_tab(target, value),
                "close_tab" => client.close_tab(target),
                "rename_pane" => client.rename_pane(target, Some(value)),
                "clear_pane_name" => client.rename_pane(target, None),
                "split_right" => client.split_pane(target, workspace, true),
                "split_down" => client.split_pane(target, workspace, false),
                "swap_panes" => client.swap_panes(target, workspace),
                "zoom_pane" => client.zoom_pane(target),
                "close_pane" => client.close_pane(target),
                "reload_config" => client.reload_config(),
                _ => Err("Unknown stock Herdr action".into()),
            }
        })();
        self.navigation_result(ui, result);
    }
}

// Focus, status and title changes update existing navigation items. Only actual
// structural changes need a new model, not every server navigation notification.
pub(crate) fn update_navigation_entries<T: Clone + PartialEq + 'static>(
    model: &mut ModelRc<T>,
    values: Vec<T>,
) -> bool {
    if let Some(retained) = model.as_any().downcast_ref::<VecModel<T>>() {
        if retained.row_count() == values.len() {
            for (index, value) in values.into_iter().enumerate() {
                if retained.row_data(index).as_ref() != Some(&value) {
                    retained.set_row_data(index, value);
                }
            }
            return false;
        }
    }
    *model = ModelRc::new(VecModel::from(values));
    true
}
