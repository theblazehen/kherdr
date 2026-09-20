slint::slint! {
    export { HostView, OpenSessionView, HostSessionView, ConnectionDraft, KeyView, TrustedHostView, FileView, TabView, SidebarRow, MachineRow, CellView, BackgroundRun, RowView, ImageView, TouchKey, KeyRow } from "ui/models.slint";
    import { HostView, OpenSessionView, HostSessionView, ConnectionDraft, KeyView, TrustedHostView, FileView, TabView, SidebarRow, MachineRow, CellView, BackgroundRun, RowView, ImageView, TouchKey, KeyRow } from "ui/models.slint";
    export { PointerTiming, KeyboardInteraction } from "ui/keyboard.slint";
    import { TouchKeyboard, PointerTiming, KeyboardInteraction } from "ui/keyboard.slint";
    import { Theme } from "ui/theme.slint";
    import { CellGlyph, TouchButton, IconButton, Glyph, SessionCheck, StateGlyph, TabVisibilityObserver, NativeTab, PaneRow, ConnectionField } from "ui/controls.slint";
    enum Route { terminal, entry, hosts, host-actions, sessions, connection, setup, session-name, credentials, keys, key-editor, trusted-hosts, key-files, key-details, key-picker, menu, settings, help, panes, machines, actions, worktree-actions, rename, confirm-action }
    enum ReadingGesture { idle, candidate, swipe, select }

    export component AppWindow inherits Window {
        in property <[KeyView]> key_entries;
        in property <[TrustedHostView]> trusted_hosts;
        in property <[FileView]> key_files;
        out property <Route> route: Route.entry;
        private property <Route> credentials-return: Route.hosts;
        private property <bool> terminal-keyboard: false;
        private property <bool> credentials-visible: root.route == Route.credentials || root.route == Route.keys
            || root.route == Route.key-editor || root.route == Route.trusted-hosts || root.route == Route.key-files
            || root.route == Route.key-details || root.route == Route.key-picker;
        in-out property <bool> auth_busy;
        in-out property <string> auth_error;
        in-out property <int> auth_generation;
        in-out property <string> key_id;
        in-out property <string> key_name;
        out property <string> key_source;
        in-out property <string> key_material;
        in-out property <string> key_passphrase;
        in-out property <string> key_public_key;
        in-out property <string> key_fingerprint;
        in-out property <string> key_path;
        in-out property <string> key_directory;
        out property <bool> auth_prompt_visible;
        out property <int> auth_prompt_kind;
        in-out property <string> auth_prompt_title;
        in-out property <string> auth_prompt_detail;
        in-out property <string> auth_fingerprint;
        in-out property <string> auth_previous_fingerprint;
        in-out property <string> auth_secret;
        in-out property <bool> auth_remember_password: false;
        in property <bool> auth_password_saving_allowed: false;
        callback forget-saved-passwords();
        out property <bool> local_editor_active: root.application_active && !root.auth_busy && root.auth-removing == ""
            && (root.auth_prompt_visible ? root.auth_prompt_kind >= 2
                : root.route == Route.session-name || (!root.setup_visible && (root.action_renaming || root.route == Route.key-editor || root.route == Route.key-files || (!root.credentials-visible && root.editing_connection))));
        out property <bool> local_copy_allowed: root.local_editor_active && !root.auth_prompt_visible
            && (root.route != Route.key-editor || root.auth-field < 2);
        callback open-auth-manager();
        callback create-key();
        callback edit-key(id: string);
        callback view-key(id: string);
        callback save-key();
        callback delete-key(id: string);
        callback copy-public-key();
        callback choose-key(path: string);
        callback browse-key-files(path: string);
        callback choose-key-file(path: string);
        callback open-trusted-hosts();
        callback remove-trusted-host(id: string);
        callback answer-auth(approved: bool);
        callback restore-editor-focus();
        restore-editor-focus => { root.focus-editor(); }
        in property <[HostView]> host_entries;
        in property <[OpenSessionView]> open_sessions;
        in property <[HostSessionView]> host_sessions;
        in property <string> current_view_id;
        in property <string> current_session_label;
        in property <string> current_session_name;
        in property <bool> setup_inspected;
        in property <bool> setup_available;
        callback open-host-session(host: string, name: string);
        callback copy-fingerprint();
        private property <bool> verification-help: false;
        callback switch-session(id: string);
        callback toggle-machine(id: string);
        callback navigate-terminal(endpoint: string, workspace: string, pane: string);
        callback set-agent-priority(priority: bool);
        in-out property <bool> agents_priority: false;
        in property <int> attention_count;
        in property <bool> attention_unknown: true;
        callback select-pane(id: string);
        in-out property <string> host_detail_id;
        out property <bool> sessions_visible: root.route == Route.sessions;
        out property <bool> key_selection: root.credentials-return == Route.connection;
        in property <string> selected_key_name;
        callback open-host(id: string);
        callback open-shell(id: string);
        callback resume-session(id: string);
        callback close-session(id: string);
        callback disconnect-host(id: string);
        callback setup-herdr(id: string);
        callback new-herdr-session(id: string);
        in property <HostView> host_detail;
        in-out property <Route> session_name_return: Route.hosts;
        callback set-session-visible(host: string, name: string, visible: bool);
        callback inspect-session-name(name: string);
        in-out property <string> new_session_name;
        in property <[string]> setup_sessions;
        out property <bool> setup_visible: root.route == Route.setup || root.route == Route.session-name;
        in-out property <string> setup_host_id;
        in-out property <string> setup_host_name;
        in-out property <string> setup_session_name;
        in-out property <string> setup_binary;
        in-out property <string> setup_detail;
        in-out property <bool> setup_busy: false;
        in-out property <bool> setup_start_allowed: false;
        in-out property <bool> setup_confirmation: false;
        callback inspect-setup();
        callback confirm-setup();
        callback cancel-setup();
        in property <string> current_connection_id;
        in property <bool> current_endpoint_local;
        in property <string> current_connection_name;
        in property <string> current_connection_detail;
        in-out property <ConnectionDraft> connection_draft;
        in-out property <string> connection_error;
        out property <bool> connections_visible: root.route == Route.entry || root.route == Route.hosts || root.route == Route.host-actions || root.route == Route.sessions || root.route == Route.connection || root.setup_visible || root.credentials-visible;
        out property <bool> editing_connection: root.route == Route.connection;
        in-out property <int> local_editor_generation: 0;
        // Read native focus directly. Deferred has-focus notifications only
        // remember/scroll the field; they cannot invalidate an admitted edit.
        out property <int> editor_target: name-field.input-focused ? 1 : host-field.input-focused ? 2
            : user-field.input-focused ? 3 : port-field.input-focused ? 4
            : herdr-session-field.input-focused ? 5 : herdr-binary-field.input-focused ? 6
            : keepalive-field.input-focused ? 7 : key-name-field.input-focused ? 8
            : key-path-field.input-focused ? 9 : key-material-field.input-focused ? 10
            : key-passphrase-field.input-focused ? 11 : directory-field.input-focused ? 12
            : prompt-secret.input-focused ? 13 : action-rename-field.input-focused ? 14 : new-session-field.input-focused ? 15 : 0;
        callback new-connection();
        callback edit-connection(id: string);
        callback save-connection(draft: ConnectionDraft);
        callback remove-connection(id: string);
        in property <[TabView]> tabs;
        in property <[MachineRow]> machine_rows;
        in property <[SidebarRow]> agent_rows;
        in property <[SidebarRow]> pane_rows;
        in property <[RowView]> rows;
        in property <[ImageView]> images_below_background;
        in property <[ImageView]> images_below_text;
        in property <[ImageView]> images_above_text;
        in property <[KeyRow]> keyboard_rows;
        in property <int> text_size: 0;
        in property <bool> input_ready: false;
        out property <bool> application_active: true;
        out property <bool> remote_input_allowed: root.can-input && !root.modal;
        in property <bool> connected: false;
        in property <bool> connecting_or_retrying: false;
        in property <bool> selection_pending: false;
        in property <string> connection_status;
        in property <string> capability_status: "";
        in property <string> selected_name;
        in property <string> selected_workspace_label;
        in property <SidebarRow> focused_pane;
        in property <string> focused_pane_id;
        in property <string> focused_workspace_id;
        in property <color> terminal_background: Theme.paper;
        in property <float> cell_width;
        in property <float> cell_height;
        in property <float> terminal_font_size;
        in property <string> terminal_font_family;
        in property <int> cursor_column;
        in property <int> cursor_row;
        in property <int> cursor_span: 1;
        in property <CellView> cursor_cell;
        in property <bool> cursor_visible: false;
        in property <int> cursor_style;
        in property <int> selection_start: -1;
        in property <int> selection_end: -1;
        in property <int> columns;
        out property <bool> keyboard_visible: false;
        out property <bool> menu_visible: root.route == Route.menu || root.route == Route.settings || root.route == Route.help || root.route == Route.panes;
        out property <bool> machines_visible: root.route == Route.machines
            || ((root.action_visible || root.route == Route.confirm-action) && root.action-return == Route.machines);
        out property <float> terminal_width: viewport.width / 1phx;
        out property <float> terminal_height: viewport.height / 1phx;
        callback geometry(width: float, height: float);
        callback set-text-size(index: int);
        callback select-tab(tab: string);
        callback stock-action(action: string, target: string, workspace: string, value: string);
        callback new-tab();
        callback new-workspace();
        callback key(id: string);
        callback keyboard-text(value: string);
        callback terminal-key(text: string, control: bool, alt: bool, shift: bool);
        callback terminal-tap(column: int, row: int);
        callback paste();
        callback management-closed();
        callback copy();
        callback reconnect();
        callback disconnect();
        callback quit();
        in property <string> system-time: "—";
        in property <string> system-battery: "—";
        callback system-settings();
        callback scroll(up: bool, ticks: int);
        callback select-start(column: int, row: int);
        callback select-move(column: int, row: int);

        title: "L:A_N:application_ID:net.fabiszewski.kherdr_PC:N_O:URL";
        background: Theme.paper;
        default-font-family: "Amazon Ember";
        default-font-size: Theme.type-body;
        // The explicitly positioned content below does not size the window:
        // Tab content must not feed back into window width.
        // Reserve one readable tab, four fixed icon controls, and the system tray.
        min-width: Theme.tab-width + Theme.chrome-height * 4 + Theme.unit * 6 + 88px;
        // Keyboard/header growth consumes the viewport, not the window size.
        min-height: 0px;
        preferred-width: 1236px;
        preferred-height: 1648px;
        forward-focus: terminal-input;

        private property <bool> has-selection: root.selection_start >= 0 && root.selection_end >= 0;
        private property <bool> can-input: root.application_active && root.input_ready && root.connected && !root.selection_pending;
        private property <bool> management_header: root.connections_visible || root.editing_connection || root.setup_visible || root.credentials-visible || root.auth_prompt_visible;
        private property <bool> action_visible: root.route == Route.actions || root.route == Route.worktree-actions || root.route == Route.rename;
        private property <bool> action_renaming: root.route == Route.rename;
        private property <Route> action-return: Route.terminal;
        private property <Route> editor-return: Route.actions;
        private property <Route> confirm-return: Route.actions;
        private property <string> closing-ssh;
        private property <int> action_kind: 0;
        private property <string> action_id: "";
        private property <string> action_workspace: "";
        private property <string> action_name: "";
        private property <string> action_value: "";
        private property <string> action_edit_action: "";
        private property <bool> action_can_zoom;
        private property <bool> action_zoomed;
        private property <bool> action_can_swap;
        private property <bool> action_custom_label;
        private property <bool> action_worktree_linked: false;
        private property <string> action_worktree_key:"";
        private property <bool> action_has_worktree_children:false;
        private property <bool> action_group_collapsed:false;
        private property <string> confirming_action: "";
        private property <bool> modal: root.route != Route.terminal || root.auth_prompt_visible;
        private property <int> auth-field: 0;
        private property <string> auth-removing;
        private property <string> auth-removing-name;
        private property <bool> auth-removing-host: false;
        private property <bool> auth-controls: root.application_active && !root.auth_busy && !root.auth_prompt_visible && root.auth-removing == "";
        private property <bool> advanced: false;
        private property <string> removing-connection: "";
        private property <string> removing-name: "";
        private property <int> editor-field: 0;
        private property <string> dismissed-status: "";
        private property <bool> visible-status: root.has-status-detail && root.connection_status != root.dismissed-status
            && root.connection_status != "Connected" && root.connection_status != "Paste queued"
            && root.connection_status != "Switching terminal" && root.connection_status != "Connected — starting terminal";
        // Touch keys share terminal input ownership even when a key takes focus.
        private property <bool> cursor-active: root.can-input && !root.modal;
        private property <bool> cursor-selected: root.has-selection
            && root.cursor_row * root.columns + root.cursor_column + max(1, root.cursor_span) - 1
                >= min(root.selection_start, root.selection_end)
            && root.cursor_row * root.columns + root.cursor_column
                <= max(root.selection_start, root.selection_end);
        private property <color> cursor-paper: root.cursor-selected ? Theme.ink : root.cursor_cell.background;
        private property <color> cursor-ink: root.cursor-paper == Theme.ink ? Theme.paper : Theme.ink;
        private property <string> session-status: root.connecting_or_retrying ? "Connecting…" : !root.connected ? "Disconnected"
            : root.selection_pending ? "Switching agent…" : root.input_ready ? "Connected" : "Connecting…";
        private property <bool> has-status-detail: root.connection_status != ""
            && root.connection_status != root.session-status;
        // Physical lengths need this window's scale, unavailable in a global.
        private property <length> tap-slop: 8phx;
        private property <length> hold-slop: 20phx;
        private property <length> page-distance: 60phx;

        // Outputs and callback describe the cell viewport, not the whole window.
        // Header/keyboard geometry is fixed; navigation and menus only overlay.
        changed terminal_width => { selection.cancel-gesture(); root.geometry(root.terminal_width, root.terminal_height); }
        changed terminal_height => { selection.cancel-gesture(); root.geometry(root.terminal_width, root.terminal_height); }
        init => { root.geometry(root.terminal_width, root.terminal_height); }
        changed keyboard_visible => { root.geometry(root.terminal_width, root.terminal_height); }
        // Only these synchronous transitions may mutate route/prompt/keyboard.
        // Fence before assignment: Slint changed handlers run later, potentially
        // after an async operation has already captured its admission identity.
        callback cancel-input();
        cancel-input => { root.fence-input(); }
        function fence-input() {
            selection.cancel-gesture();
            root.local_editor_generation += 1;
            KeyboardInteraction.generation += 1;
        }
        callback set-keyboard(show: bool);
        set-keyboard(show) => {
            if root.keyboard_visible == show { return; }
            root.fence-input();
            root.keyboard_visible = show;
            if root.route == Route.terminal && !root.auth_prompt_visible { root.terminal-keyboard = show; }
            root.restore-focus();
        }
        callback application-activity(active: bool);
        application-activity(active) => {
            if root.application_active == active { return; }
            root.fence-input();
            root.application_active = active;
            if !active { root.clear-auth-secrets(); }
            else { root.restore-focus(); }
        }
        callback present-auth-prompt(kind: int);
        present-auth-prompt(kind) => {
            root.fence-input(); root.auth_generation += 1;
            root.clear-auth-secrets();
            root.verification-help = false;
            root.auth_prompt_kind = kind;
            root.auth_prompt_visible = true;
            if kind >= 2 { root.set-keyboard(true); }
            root.restore-focus();
        }
        callback hide-auth-prompt();
        hide-auth-prompt => {
            if !root.auth_prompt_visible { return; }
            root.fence-input(); root.auth_generation += 1;
            root.clear-auth-secrets();
            root.auth_prompt_visible = false;
            if root.route == Route.terminal { root.set-keyboard(root.terminal-keyboard); }
            root.restore-focus();
        }
        callback change-key-source(source: string);
        change-key-source(source) => {
            if root.key_source == source { return; }
            root.fence-input(); root.auth_generation += 1;
            root.key_source = source; root.auth-field = 0;
            root.key_material = ""; root.key_passphrase = "";
            key-passphrase-field.revealed = false; key-material-field.revealed = false;
            root.restore-focus();
        }
        callback navigate(next: Route);
        navigate(next) => {
            if root.route == next || root.auth_busy { return; }
            let was-management = root.connections_visible;
            let was-credentials = root.credentials-visible;
            root.fence-input(); root.auth_generation += 1;
            root.set-auth-removing(""); root.removing-connection = "";
            root.closing-ssh = "";
            if root.route == Route.terminal { root.terminal-keyboard = root.keyboard_visible; }
            if next != Route.key-editor && next != Route.key-files { root.key_material = ""; root.key_passphrase = ""; }
            if next == Route.connection && !was-credentials { root.advanced = false; root.editor-field = 1; }
            root.auth-field = 0;
            key-passphrase-field.revealed = false; key-material-field.revealed = false;
            root.route = next;
            if root.local_editor_active { root.set-keyboard(true); }
            else if next == Route.terminal { root.set-keyboard(root.terminal-keyboard); }
            // Non-editor pages cover inactive keys without changing PTY geometry.
            root.restore-focus();
            if was-management && !root.connections_visible { root.management-closed(); }
        }
        function restore-focus() {
            if !root.application_active { return; }
            if root.local_editor_active { root.focus-editor(); }
            else if root.auth_prompt_visible { prompt-cancel.focus(); }
            else if root.credentials-visible { auth-close.focus(); }
            else if root.connections_visible { manager-close.focus(); }
            else if !root.modal { terminal-input.focus(); }
        }
        function set-auth-removing(id: string) {
            if root.auth-removing == id { return; }
            root.auth_generation += 1; root.fence-input();
            root.auth-removing = id;
        }
        function clear-auth-secrets() {
            root.auth_secret = ""; root.key_passphrase = ""; root.key_material = "";
            root.auth_remember_password = false;
            prompt-secret.revealed = false; key-passphrase-field.revealed = false; key-material-field.revealed = false;
        }
        function auth-field-focused(index: int, y: length, height: length) {
            root.auth-field = index;
            if y + key-editor-scroll.viewport-y < 0px { key-editor-scroll.viewport-y = -y; }
            else if y + height + key-editor-scroll.viewport-y > key-editor-scroll.height {
                key-editor-scroll.viewport-y = min(0px, key-editor-scroll.height - y - height);
            }
        }
        function focus-editor() {
            if !root.local_editor_active { return; }
            // Re-focusing a TextInput can reset its native selection. Clipboard
            // actions retain focus, so restore only when it actually moved.
            if !root.auth_prompt_visible && root.route == Route.session-name {
                if !new-session-field.input-focused { new-session-field.focus(); }
                return;
            }
            if !root.auth_prompt_visible && root.action_renaming {
                if !action-rename-field.input-focused { action-rename-field.focus(); }
                return;
            }
            if root.auth_prompt_visible && prompt-secret.input-focused { return; }
            if !root.auth_prompt_visible && root.route == Route.key-files && directory-field.input-focused { return; }
            if !root.auth_prompt_visible && root.route == Route.key-editor
                && (key-name-field.input-focused || key-path-field.input-focused
                    || key-material-field.input-focused || key-passphrase-field.input-focused) { return; }
            if !root.auth_prompt_visible && !root.credentials-visible
                && (name-field.input-focused || host-field.input-focused || user-field.input-focused
                    || port-field.input-focused || keepalive-field.input-focused
                    || herdr-session-field.input-focused || herdr-binary-field.input-focused) { return; }
            if root.auth_prompt_visible { prompt-secret.focus(); return; }
            if root.route == Route.key-files { directory-field.focus(); return; }
            if root.route == Route.key-editor {
                if root.auth-field == 1 && root.key_source == "file" { key-path-field.focus(); }
                else if root.auth-field == 2 && root.key_source == "paste" { key-material-field.focus(); }
                else if root.auth-field == 3 && root.key_id == "" { key-passphrase-field.focus(); }
                else { key-name-field.focus(); }
                return;
            }
            if !root.editing_connection { return; }
            if root.editor-field == 0 { if root.advanced { name-field.focus(); } else { host-field.focus(); } }
            else if root.editor-field == 1 { host-field.focus(); }
            else if root.editor-field == 2 { user-field.focus(); }
            else if root.editor-field == 3 { port-field.focus(); }
            else if root.editor-field == 4 { key-picker.focus(); }
            else if root.editor-field == 6 { herdr-session-field.focus(); }
            else if root.editor-field == 7 { herdr-binary-field.focus(); }
            else { keepalive-field.focus(); }
        }
        function field-focused(index: int) {
            root.editor-field = index;
        }
        function open-connections() {
            root.host_detail_id = "";
            root.navigate(Route.hosts);
        }
        function open-action(kind: int, id: string, workspace: string, name: string) {
            root.action-return = root.route;
            root.action_kind = kind; root.action_id = id; root.action_workspace = workspace;
            root.action_name = name; root.action_value = name;
            root.action_edit_action = ""; root.action_worktree_linked = false;
            root.action_worktree_key="";root.action_has_worktree_children=false;root.action_group_collapsed=false;
            root.navigate(Route.actions);
        }
        function open-pane-actions(pane: SidebarRow) {
            root.open-action(3, pane.pane_id, pane.workspace_id, pane.resource_name);
            root.action_can_zoom = pane.can_zoom; root.action_zoomed = pane.zoomed;
            root.action_can_swap = pane.can_swap; root.action_custom_label = pane.custom_label;
        }
        function edit-resource(action: string, value: string) {
            root.editor-return = root.route;
            root.action_edit_action = action; root.action_value = value;
            root.navigate(Route.rename);
        }
        function confirm-resource(action: string) {
            root.confirm-return = root.route; root.confirming_action = action;
            root.navigate(Route.confirm-action);
        }
        function management-back() {
            if root.auth_prompt_visible { root.clear-auth-secrets(); root.answer-auth(false); return; }
            if root.auth_busy { return; }
            if root.auth-removing != "" { root.set-auth-removing(""); auth-close.focus(); return; }
            if root.removing-connection != "" { root.removing-connection = ""; return; }
            if root.closing-ssh != "" { root.closing-ssh = ""; return; }
            if root.route == Route.host-actions { root.navigate(Route.hosts); return; }
            if root.route == Route.session-name {
                if root.session_name_return == Route.hosts { root.open-host(root.host_detail_id); }
                else { root.navigate(root.session_name_return); }
                return;
            }
            if root.setup_visible { if root.setup_confirmation { root.setup_confirmation = false; } else { root.cancel-setup(); } return; }
            if root.route == Route.confirm-action { root.navigate(root.confirm-return); return; }
            if root.route == Route.rename { root.navigate(root.editor-return); return; }
            if root.route == Route.worktree-actions { root.navigate(Route.actions); return; }
            if root.route == Route.actions { root.navigate(root.action-return); return; }
            if root.route == Route.sessions { root.host_detail_id = ""; root.navigate(Route.hosts); return; }
            if root.route == Route.key-files { root.navigate(Route.key-editor); }
            else if root.route == Route.keys || root.route == Route.trusted-hosts { root.navigate(Route.credentials); }
            else if root.route == Route.key-editor || root.route == Route.key-details { root.navigate(root.key_selection ? Route.key-picker : Route.keys); }
            else if root.route == Route.credentials || root.route == Route.key-picker { root.navigate(root.credentials-return); }
            else if root.editing_connection { if root.host_detail_id != "" { root.open-host(root.host_detail_id); } else { root.navigate(Route.hosts); } }
            else if root.host_detail_id != "" && root.route == Route.hosts { root.cancel-setup(); root.host_detail_id = ""; }
            else if root.menu_visible && root.route != Route.menu { root.navigate(Route.menu); }
            else if root.connections_visible && root.open_sessions.length == 0 { return; }
            else { root.dismiss-overlays(); }
        }
        function return-to-terminal() {
            if root.auth_busy { return; }
            if root.auth_prompt_visible { root.clear-auth-secrets(); root.answer-auth(false); }
            if root.setup_visible { root.setup_confirmation = false; root.cancel-setup(); }
            root.dismiss-overlays();
        }
        function open-credentials() {
            root.credentials-return = Route.hosts;
            root.open-auth-manager(); root.navigate(Route.credentials);
        }
        function open-key-picker() {
            root.credentials-return = Route.connection;
            root.open-auth-manager();
        }
        function dismiss-overlays() {
            if root.auth_prompt_visible { root.clear-auth-secrets(); root.answer-auth(false); return; }
            if root.setup_visible { root.setup_confirmation = false; root.cancel-setup(); return; }
            root.navigate(Route.terminal);
        }

        pure function pointer-column(x: length) -> int {
            return max(0, min(max(0, root.columns - 1), floor(x / (max(1, root.cell_width) * 1phx))));
        }
        pure function pointer-row(y: length) -> int {
            return max(0, min(max(0, floor(root.terminal_height / max(1, root.cell_height)) - 1),
                floor(y / (max(1, root.cell_height) * 1phx))));
        }

        terminal-input := FocusScope {
            // An explicitly positioned host keeps descendant layout constraints
            // out of Window's implicit layout info, while following native resizes.
            x: 0px;
            y: 0px;
            width: root.width;
            height: root.height;
            // Capture before focused buttons: physical terminal keys must never
            // activate a latched touch key or move focus instead of being sent.
            capture-key-pressed(event) => {
                if root.modal && event.text == Key.Escape {
                    root.management-back();
                    return accept;
                }
                if root.local_editor_active && event.modifiers.control
                    && (event.text == "v" || event.text == "V") {
                    root.paste();
                    return accept;
                }
                if root.local_editor_active && event.modifiers.control
                    && (event.text == "c" || event.text == "C" || event.text == "x" || event.text == "X") {
                    // Let the focused native TextInput copy its real selection.
                    // Re-dispatching root.copy here would recursively capture Ctrl+C.
                    return root.local_copy_allowed ? reject : accept;
                }
                if root.can-input && !root.modal {
                    root.terminal-key(event.text, event.modifiers.control,
                        event.modifiers.alt, event.modifiers.shift);
                    return accept;
                }
                return reject;
            }
          VerticalLayout {
            x: 0px;
            y: 0px;
            width: root.width;
            height: root.height;
            spacing: 0px;
            Rectangle {
                height: Theme.chrome-height;
                clip: true;
                background: Theme.paper;
                manager-close := IconButton {
                    visible: root.management_header && !root.credentials-visible && !root.auth_prompt_visible;
                    x: 0px; y: 0px;
                    width: Theme.chrome-height; height: Theme.chrome-height;
                    icon: root.route == Route.hosts && root.host_detail_id == "" ? "close" : "back";
                    label: root.route == Route.hosts && root.host_detail_id == "" ? "Return to terminal" : root.route == Route.hosts ? "Back to hosts" : "Back";
                    enabled: root.auth-controls;
                    activated => { root.management-back(); }
                }
                auth-close := IconButton {
                    visible: root.credentials-visible && !root.auth_prompt_visible;
                    x: 0px; y: 0px;
                    width: Theme.chrome-height; height: Theme.chrome-height;
                    icon: "back"; label: "Back";
                    enabled: root.auth-controls;
                    activated => { root.management-back(); }
                }
                if root.management_header: Text {
                    x: root.auth_prompt_visible ? Theme.inset : Theme.chrome-height + Theme.inset;
                    width: parent.width - self.x - Theme.chrome-height * 2 - Theme.inset - 88px;
                    height: parent.height;
                    text: root.auth_prompt_visible ? (root.auth_prompt_kind < 2 ? "Verify host" : "Sign in")
                        : root.route == Route.session-name ? "New session · " + root.setup_host_name
                        : root.setup_visible ? "Herdr · " + root.setup_host_name
                        : root.editing_connection ? (root.connection_draft.id == "" ? "Connection details" : "Connection settings")
                        : root.route == Route.credentials ? "Credentials"
                        : root.route == Route.key-picker ? "Choose SSH key"
                        : root.route == Route.keys ? "SSH keys"
                        : root.route == Route.trusted-hosts ? "Trusted server identities"
                        : root.route == Route.key-files ? "Import key file"
                        : root.route == Route.key-details ? "SSH key details"
                        : root.route == Route.key-editor ? (root.key_id == "" ? "Add SSH key" : "Edit SSH key")
                        : root.sessions_visible ? "Shown sessions"
                        : root.route == Route.entry ? "Open a terminal"
                        : root.host_detail_id != "" ? root.host_detail.name : "Hosts";
                    color: Theme.ink; font-weight: 700; vertical-alignment: center;
                    wrap: no-wrap; overflow: elide;
                }
                if root.management_header && root.local_editor_active: IconButton {
                    x: parent.width - Theme.chrome-height - 88px; width: Theme.chrome-height; height: Theme.chrome-height;
                    icon: "keyboard"; label: root.keyboard_visible ? "Hide keyboard" : "Show keyboard";
                    active: root.keyboard_visible;
                    activated => { root.set-keyboard(!root.keyboard_visible); }
                }
                if root.route == Route.hosts && root.host_detail_id == "" && !root.auth_prompt_visible: IconButton {
                    x: parent.width - Theme.chrome-height * 2 - 88px; width: Theme.chrome-height; height: Theme.chrome-height;
                    icon: "plus"; label: "Add host"; enabled: root.auth-controls;
                    activated => { root.new-connection(); }
                }
                if (root.route == Route.hosts || root.route == Route.host-actions) && !root.auth_prompt_visible: IconButton {
                    x: parent.width - Theme.chrome-height - 88px; width: Theme.chrome-height; height: Theme.chrome-height;
                    icon: "more"; label: root.host_detail_id == "" ? "Host manager actions" : "Actions for " + root.host_detail.name;
                    enabled: root.auth-controls && root.removing-connection == "";
                    active: root.route == Route.host-actions;
                    activated => { root.navigate(root.route == Route.host-actions ? Route.hosts : Route.host-actions); }
                }
                if !root.management_header: HorizontalLayout {
                x: 0px;
                width: parent.width - 88px;
                height: Theme.chrome-height;
                padding-left: Theme.unit;
                padding-right: Theme.unit;
                spacing: Theme.unit;
                Rectangle {
                    width: Theme.chrome-height; height: Theme.chrome-height;
                    IconButton {
                        width: parent.width; height: parent.height;
                        label: "Machines, " + (root.current_endpoint_local ? "This Kindle" : root.current_connection_name)
                            + ", " + root.current_session_name + (root.selected_workspace_label == "" ? "" : "/" + root.selected_workspace_label)
                            + (root.attention_unknown ? ", some agent states unavailable" : ", " + root.attention_count + " agents need input");
                        active: root.machines_visible;
                        background: root.machines_visible ? Theme.selection : Theme.clear;
                        activated => { root.navigate(root.machines_visible ? Route.terminal : Route.machines); }
                    }
                    Glyph { x: (parent.width - self.width) / 2; y: (parent.height - self.height) / 2; name: "workspaces"; }
                    Rectangle {
                        visible: root.attention_unknown || root.attention_count > 0;
                        x: 32px; y: Theme.unit; width: 40px; height: 40px; border-radius: 20px; background: Theme.ink;
                        Text { text: root.attention_unknown ? "?" : root.attention_count > 9 ? "9+" : "" + root.attention_count;
                            color: Theme.paper; font-size: Theme.type-small; font-weight: 700; horizontal-alignment: center; vertical-alignment: center; }
                    }
                }
                tab-strip := Flickable {
                    horizontal-stretch: 1;
                    min-width: 0px;
                    max-width: root.width;
                    preferred-width: 0px;
                    height: Theme.chrome-height;
                    viewport-width: max(self.width, tab-row.width);
                    viewport-height: self.height;
                    changed viewport-width => { self.viewport-x = min(0px, max(self.width - self.viewport-width, self.viewport-x)); }
                    tab-row := HorizontalLayout {
                        x: 0px;
                        y: 0px;
                        width: self.preferred-width;
                        height: parent.height;
                        spacing: 0px;
                    for tab in root.tabs: NativeTab {
                        height: Theme.chrome-height;
                        tab: tab;
                        // Reveal on selection/geometry changes, not ordinary scrolling.
                        TabVisibilityObserver {
                            selected: tab.selected;
                            left: parent.x;
                            tab-width: parent.width;
                            viewport-width: tab-strip.width;
                            reveal(left, width) => {
                                tab-strip.viewport-x = min(0px, max(tab-strip.width - tab-strip.viewport-width,
                                    min(-left, max(tab-strip.viewport-x, tab-strip.width - left - width))));
                            }
                        }
                        host_name: root.current_session_label;
                        activated => {
                            if !root.modal && !self.tab.selected {
                                selection.cancel-gesture();
                                root.dismiss-overlays();
                                root.select-tab(self.tab.tab_id);
                            }
                        }
                        menu-requested => { if !root.modal { root.open-action(2, self.tab.tab_id, "", self.tab.name); } }
                    }
                    }
                }
                IconButton {
                    width: Theme.chrome-height;
                    height: Theme.chrome-height;
                    icon: "plus";
                    label: root.current_endpoint_local && root.connected ? "New local shell" : "New tab";
                    enabled: root.connected && !root.modal;
                    activated => { root.dismiss-overlays(); root.new-tab(); }
                }
                IconButton {
                    width: Theme.chrome-height;
                    height: Theme.chrome-height;
                    icon: "keyboard";
                    label: root.keyboard_visible ? "Hide keyboard" : "Show keyboard";
                    active: root.keyboard_visible;
                    enabled: !root.modal || root.local_editor_active;
                    activated => { root.set-keyboard(!root.keyboard_visible); }
                }
                IconButton {
                    width: Theme.chrome-height;
                    height: Theme.chrome-height;
                    icon: "menu";
                    label: "Menu: " + root.session-status;
                    active: root.menu_visible;
                    enabled: !root.connections_visible && !root.editing_connection;
                    activated => { root.navigate(root.menu_visible ? Route.terminal : Route.menu); }
                }
            }
                IconButton {
                    x: parent.width - 88px; width: 88px; height: Theme.chrome-height;
                    label: "Quick Settings, " + root.system-time + ", battery " + root.system-battery;
                    activated => { root.system-settings(); }
                    VerticalLayout {
                        padding: 4px; spacing: 0px; alignment: center;
                        Text { text: root.system-time; color: Theme.ink; font-size: Theme.type-small; horizontal-alignment: center; }
                        Text { text: root.system-battery; color: Theme.ink; font-size: Theme.type-small; horizontal-alignment: center; }
                    }
                }
            }
            viewport := Rectangle {
                background: root.terminal_background;
                // Terminal geometry depends only on the window and fixed chrome,
                // never on the preferred or maximum size of an active overlay.
                height: max(0px, root.height - Theme.chrome-height
                    - (root.keyboard_visible ? Theme.keyboard-height : 0px));
                min-width: 0px;
                clip: true;
                for sprite in root.images_below_background: Image {
                    x: sprite.x * 1phx; y: sprite.y * 1phx;
                    width: sprite.width * 1phx; height: sprite.height * 1phx;
                    source: sprite.source;
                }
                for row in root.rows: Rectangle {
                    y: row.index * root.cell_height * 1phx;
                    height: root.cell_height * 1phx;
                    for run in row.backgrounds: Rectangle {
                        x: run.column * root.cell_width * 1phx;
                        width: run.span * root.cell_width * 1phx;
                        height: root.cell_height * 1phx;
                        background: run.background;
                    }
                }
                for sprite in root.images_below_text: Image {
                    x: sprite.x * 1phx; y: sprite.y * 1phx;
                    width: sprite.width * 1phx; height: sprite.height * 1phx;
                    source: sprite.source;
                }
                for row in root.rows: Rectangle {
                    y: row.index * root.cell_height * 1phx;
                    height: root.cell_height * 1phx;
                    for cell in row.glyphs: CellGlyph {
                        visible: cell.span > 0;
                        x: cell.column * root.cell_width * 1phx;
                        width: max(1, cell.span) * root.cell_width * 1phx;
                        height: root.cell_height * 1phx;
                        cell: cell;
                        ink: cell.foreground;
                        family: root.terminal_font_family;
                        size: root.terminal_font_size;
                    }
                }
                for sprite in root.images_above_text: Image {
                    x: sprite.x * 1phx; y: sprite.y * 1phx;
                    width: sprite.width * 1phx; height: sprite.height * 1phx;
                    source: sprite.source;
                }
                // Selection remains legible even over an above-text image.
                if root.has-selection: Rectangle {
                    for row in root.rows: Rectangle {
                        y: row.index * root.cell_height * 1phx;
                        height: root.cell_height * 1phx;
                        for cell in row.cells: CellGlyph {
                            private property <int> cell-index: row.index * root.columns + cell.column;
                            visible: self.cell-index + max(1, cell.span) - 1 >= min(root.selection_start, root.selection_end)
                                && self.cell-index <= max(root.selection_start, root.selection_end);
                            x: cell.column * root.cell_width * 1phx;
                            width: max(1, cell.span) * root.cell_width * 1phx;
                            height: root.cell_height * 1phx;
                            background: Theme.ink;
                            cell: cell;
                            ink: Theme.paper;
                            family: root.terminal_font_family;
                            size: root.terminal_font_size;
                        }
                    }
                }
                // One static overlay: active blocks invert the retained glyph;
                // inactive cursors keep their requested shape but only its outline.
                Rectangle {
                    visible: root.cursor_visible && root.cell_width > 0 && root.cell_height > 0;
                    x: root.cursor_column * root.cell_width * 1phx;
                    y: root.cursor_row * root.cell_height * 1phx;
                    width: max(1, root.cursor_span) * root.cell_width * 1phx;
                    height: root.cell_height * 1phx;
                    CellGlyph {
                        visible: root.cursor-active && root.cursor_style == 0;
                        width: parent.width;
                        height: parent.height;
                        background: root.cursor-ink;
                        cell: root.cursor_cell;
                        // Do not reveal intentionally background-colored text.
                        ink: root.cursor_cell.foreground == root.cursor_cell.background
                            ? root.cursor-ink : root.cursor-paper;
                        family: root.terminal_font_family;
                        size: root.terminal_font_size;
                    }
                    Rectangle {
                        visible: !root.cursor-active || root.cursor_style != 0;
                        y: root.cursor_style == 2 ? parent.height - Theme.unit : 0px;
                        width: root.cursor_style == 1 ? Theme.unit : parent.width;
                        height: root.cursor_style == 2 ? Theme.unit : parent.height;
                        border-width: root.cursor-active ? 0px : 1phx;
                        border-color: root.cursor-ink;
                        background: root.cursor-active ? root.cursor-ink : Theme.clear;
                    }
                }
                selection := TouchArea {
                    enabled: root.can-input && !root.modal && root.columns > 0
                        && root.cell_width > 0 && root.cell_height > 0;
                    private property <ReadingGesture> gesture: ReadingGesture.idle;
                    private property <length> anchor-x;
                    private property <length> anchor-y;
                    private property <length> scroll-y;
                    private property <bool> vertical-swipe: false;
                    private property <bool> tap-candidate: false;
                    private property <string> gesture-pane: root.focused_pane_id;
                    private property <int> interaction-generation: KeyboardInteraction.generation;
                    // Explicit focus/reset ownership also fences retained models
                    // when the next terminal happens to have the same label.
                    changed interaction-generation => { self.cancel-gesture(); }
                    changed gesture-pane => { self.cancel-gesture(); }
                    changed enabled => { if !self.enabled { self.cancel-gesture(); } }

                    pure function beyond-hold-slop() -> bool {
                        return max(self.mouse-x - self.anchor-x, self.anchor-x - self.mouse-x) > root.hold-slop
                            || max(self.mouse-y - self.anchor-y, self.anchor-y - self.mouse-y) > root.hold-slop;
                    }
                    pure function is-vertical() -> bool {
                        return max(self.mouse-y - self.anchor-y, self.anchor-y - self.mouse-y)
                            > max(self.mouse-x - self.anchor-x, self.anchor-x - self.mouse-x);
                    }
                    function cancel-gesture() {
                        hold.stop();
                        let was-selecting = self.gesture == ReadingGesture.select;
                        self.gesture = ReadingGesture.idle;
                        self.vertical-swipe = false;
                        self.tap-candidate = false;
                        if was-selecting {  }
                    }
                    function track-gesture() {
                        // A moved pointer cannot become a tap by returning to its anchor.
                        // Physical-pixel tolerances admit touch jitter, not drag clicks.
                        if max(self.mouse-x - self.anchor-x, self.anchor-x - self.mouse-x) > root.tap-slop
                            || max(self.mouse-y - self.anchor-y, self.anchor-y - self.mouse-y) > root.tap-slop {
                            self.tap-candidate = false;
                        }
                        if !self.enabled {
                            self.cancel-gesture();
                        } else if self.gesture == ReadingGesture.candidate && self.beyond-hold-slop() {
                            hold.stop();
                            self.gesture = ReadingGesture.swipe;
                            // Lock the initial direction: a horizontal drag cannot page.
                            self.vertical-swipe = self.is-vertical();
                        } else if self.gesture == ReadingGesture.select {
                            root.select-move(root.pointer-column(self.mouse-x), root.pointer-row(self.mouse-y));
                        }
                        if self.enabled && self.gesture == ReadingGesture.swipe && self.vertical-swipe {
                            // Consume whole physical-distance ticks, retaining a signed
                            // remainder. Reversing first cancels that remainder.
                            let distance = self.mouse-y - self.scroll-y;
                            let ticks = floor(max(distance, -distance) / root.page-distance);
                            if ticks > 0 {
                                self.scroll-y += (distance > 0px ? 1 : -1) * ticks * root.page-distance;
                                root.scroll(distance > 0px, ticks);
                            }
                        }
                    }
                    hold := Timer {
                        interval: Theme.selection-hold;
                        running: false;
                        triggered => {
                            self.stop();
                            selection.tap-candidate = false;
                            if selection.enabled && selection.pressed
                                && selection.gesture == ReadingGesture.candidate
                                && !selection.beyond-hold-slop() {
                                selection.gesture = ReadingGesture.select;
                                root.select-start(root.pointer-column(selection.anchor-x), root.pointer-row(selection.anchor-y));
                            }
                        }
                    }
                    pointer-event(event) => {
                        if event.kind == PointerEventKind.cancel {
                            self.cancel-gesture();
                        } else if event.button == PointerEventButton.left && event.kind == PointerEventKind.down {
                            self.cancel-gesture();
                            terminal-input.focus();
                            if self.enabled {
                                self.anchor-x = self.mouse-x;
                                self.anchor-y = self.mouse-y;
                                self.scroll-y = self.mouse-y;
                                self.gesture = ReadingGesture.candidate;
                                self.tap-candidate = true;
                                hold.start();
                            }
                        } else if event.button == PointerEventButton.left && event.kind == PointerEventKind.up {
                            // Include the final coordinates even if motion was coalesced.
                            self.track-gesture();
                            let tap = self.enabled && self.gesture == ReadingGesture.candidate && self.tap-candidate;
                            let column = root.pointer-column(self.mouse-x);
                            let row = root.pointer-row(self.mouse-y);
                            self.cancel-gesture();
                            if tap { root.terminal-tap(column, row); }
                        }
                        // Secondary pointer events must not interrupt a touch swipe
                        // or open actions. Pane actions have explicit menu buttons.
                    }
                    moved => {
                        if self.pressed { self.track-gesture(); }
                    }
                    scroll-event(event) => {
                        if event.delta-y != 0px {
                            self.cancel-gesture();
                            // Native X11 wheel buttons produce 80 logical pixels per
                            // notch; preserve all notches in a coalesced event.
                            root.scroll(event.delta-y > 0px,
                                max(1, round(max(event.delta-y, -event.delta-y) / 80px)));
                            return accept;
                        }
                        return reject;
                    }
                }
                if !root.connected && !root.modal: Rectangle {
                    x: Theme.inset; y: Theme.inset; width: parent.width - Theme.inset * 2;
                    height: disconnected-content.preferred-height;
                    background: Theme.paper; border-color: Theme.separator; border-width: Theme.rule; border-radius: Theme.radius;
                    disconnected-content := VerticalLayout {
                        padding: Theme.inset; spacing: Theme.gap; alignment: start;
                        Text { text: (root.connecting_or_retrying ? "Connecting to " : "Disconnected from ") + root.current_connection_name; color: Theme.ink; font-weight: 700; wrap: word-wrap; }
                        Text { text: root.connection_status; color: Theme.secondary; font-size: Theme.type-small; wrap: word-wrap; }
                        HorizontalLayout {
                            height: Theme.touch; spacing: Theme.inset;
                            TouchButton { width: Theme.touch * 2; icon: root.connecting_or_retrying ? "close" : "play"; label: root.connecting_or_retrying ? "Stop retrying" : "Retry"; active: !root.connecting_or_retrying;
                                activated => { if root.connecting_or_retrying { root.disconnect(); } else { root.reconnect(); } } }
                            TouchButton { width: Theme.touch * 4; icon: "settings"; label: root.current_endpoint_local ? "Hosts" : "Connection settings";
                                activated => { if root.current_endpoint_local { root.open-connections(); } else { root.edit-connection(root.current_connection_id); } } }
                            Rectangle { horizontal-stretch: 1; }
                        }
                        Text { text: "Last terminal view · read-only"; color: Theme.secondary; font-size: Theme.type-legend; }
                    }
                }
                if root.connected && root.visible-status && !root.modal: Rectangle {
                    width: parent.width; height: status-strip.preferred-height; background: Theme.paper;
                    border-color: Theme.ink; border-width: Theme.rule;
                    status-strip := HorizontalLayout {
                        padding: Theme.gap;
                        Text { text: root.connection_status; color: Theme.ink; wrap: word-wrap; vertical-alignment: center; }
                        IconButton { icon: "close"; label: "Dismiss message"; activated => { root.dismissed-status = root.connection_status; terminal-input.focus(); } }
                    }
                }
                if root.has-selection && !root.modal: Rectangle {
                    y: parent.height - self.height; width: parent.width; height: Theme.touch;
                    background: Theme.paper; border-color: Theme.ink; border-width: Theme.rule;
                    HorizontalLayout {
                        TouchButton { label: "Copy selection"; activated => { root.copy(); terminal-input.focus(); } }
                        TouchButton { label: "Paste"; enabled: root.can-input; activated => { root.paste(); terminal-input.focus(); } }
                    }
                }
            }
            if root.keyboard_visible: TouchKeyboard {
                key-rows: root.keyboard_rows;
                input-ready: root.local_editor_active || (root.can-input && !root.modal);
                key(id) => {
                    if root.local_editor_active { root.key(id); }
                    else if root.can-input && !root.modal { root.key(id); terminal-input.focus(); }
                }
                text(value) => {
                    if root.local_editor_active { root.keyboard-text(value); }
                    else if root.can-input && !root.modal { root.keyboard-text(value); terminal-input.focus(); }
                }
            }
          }
          // Opaque navigation page; independent of the retained terminal viewport.
          // Covers inactive keys without resizing the remote PTY on menu changes.
          Rectangle {
              visible: root.modal;
              x: 0px; y: Theme.chrome-height;
              width: root.width;
              height: max(0px, root.height - Theme.chrome-height
                  - (root.keyboard_visible && root.local_editor_active ? Theme.keyboard-height : 0px));
              background: root.machines_visible || root.action_visible || root.route == Route.confirm-action ? Theme.clear : Theme.paper;
              clip: true;
                TouchArea { clicked => { if root.machines_visible { root.dismiss-overlays(); } } }
                if root.keyboard_visible && !root.local_editor_active: Rectangle {
                    y: max(0px, parent.height - Theme.keyboard-height);
                    width: parent.width; height: Theme.keyboard-height;
                    background: Theme.paper;
                }
                if root.machines_visible: Rectangle {
                    x: 0px; y: 0px; width: min(parent.width, Theme.sidebar-width); height: parent.height;
                    background: Theme.paper; border-width: Theme.rule; border-color: Theme.ink;
                    TouchArea { }
                    VerticalLayout {
                        x: 0px; y: 0px; width: parent.width; height: parent.height;
                        padding: Theme.gap; spacing: 0px; alignment: start;
                        HorizontalLayout {
                            height: Theme.touch;
                            Text { text: "Machines"; font-weight: 700; color: Theme.ink; vertical-alignment: center; }
                            IconButton { icon: "close"; label: "Close Machines"; activated => { root.dismiss-overlays(); } }
                        }
                        Flickable {
                            min-height: 0px; vertical-stretch: 1; viewport-width: self.width;
                            viewport-height: max(self.height, machine-content.preferred-height);
                            machine-content := VerticalLayout {
                                x: 0px; y: 0px; width: parent.width; height: self.preferred-height; alignment: start; spacing: 0px;
                                for row in root.machine_rows: Rectangle {
                                    height: row.machine ? Theme.touch : Theme.chrome-height;
                                    if row.machine: Rectangle {
                                        TouchButton {
                                            width: parent.width; height: parent.height; plain: true;
                                            icon: row.expanded ? "down" : "chevron";
                                            accessible-label: (row.expanded ? "Collapse " : "Expand ") + row.entry.label;
                                            activated => { root.toggle-machine(row.endpoint_id); }
                                        }
                                        Glyph { x: 56px; y: (parent.height - self.height) / 2; name: row.local ? "kindle" : "monitor"; }
                                        Text { x: 104px; width: parent.width - self.x - Theme.inset; height: parent.height;
                                            text: row.entry.label + (row.entry.detail == "" ? "" : " · " + row.entry.detail);
                                            color: Theme.ink; font-size: Theme.type-small; font-weight: 700; vertical-alignment: center; overflow: elide; }
                                    }
                                    if !row.machine: PaneRow {
                                        x: 40px; width: parent.width - self.x; entry: row.entry; enabled: row.enabled;
                                        activated => { root.navigate-terminal(row.endpoint_id, row.entry.workspace_id, ""); }
                                        menu-requested => {
                                            let entry = row.entry; let workspace = row.entry.workspace_id; let endpoint = row.endpoint_id;
                                            root.switch-session(endpoint);
                                            root.open-action(1, entry.workspace_id, workspace, entry.resource_name);
                                            root.action_worktree_linked = entry.worktree_linked; root.action_worktree_key = entry.worktree_key;
                                            root.action_has_worktree_children = entry.has_worktree_children; root.action_group_collapsed = entry.group_collapsed;
                                        }
                                    }
                                }
                                HorizontalLayout {
                                    padding-top: Theme.inset; padding-bottom: Theme.inset; spacing: Theme.inset;
                                    TouchButton { height: Theme.touch; icon: "plus"; label: "New workspace"; enabled: root.connected;
                                        accessible-label: "New workspace in " + root.current_session_label;
                                        activated => { root.dismiss-overlays(); root.new-workspace(); } }
                                    TouchButton { width: Theme.touch * 2.4; height: Theme.touch; icon: "menu"; label: "Menu"; activated => { root.navigate(Route.menu); } }
                                }
                                Rectangle { height: Theme.hairline; background: Theme.separator; }
                                HorizontalLayout {
                                    height: Theme.touch + Theme.inset * 2; padding-top: Theme.inset; padding-bottom: Theme.inset;
                                    Text { text: "Agents"; font-weight: 700; color: Theme.ink; vertical-alignment: center; }
                                    TouchButton { width: Theme.touch * 2.4; icon: "down"; label: root.agents_priority ? "Priority" : "Grouped";
                                        activated => { root.set-agent-priority(!root.agents_priority); } }
                                }
                                for agent in root.agent_rows: PaneRow {
                                    entry: agent; agent-row: true; show-menu: false; enabled: agent.available;
                                    activated => { root.navigate-terminal(agent.endpoint_id, agent.workspace_id, agent.pane_id); }
                                }
                            }
                        }
                    }
                }
                if root.action_visible || root.route == Route.confirm-action: TouchArea {
                    width: parent.width; height: parent.height;
                    clicked => { root.management-back(); }
                }
                Rectangle {
                    visible: root.action_visible;
                    x: Theme.inset;
                    y: Theme.inset;
                    width: min(parent.width - Theme.inset * 2, root.action_kind == 3 ? 820px : Theme.sidebar-width);
                    height: min(parent.height - Theme.inset * 2, action-content.preferred-height);
                    background: Theme.paper; border-color: Theme.ink; border-width: Theme.rule;
                    border-radius: Theme.radius;
                    TouchArea { }
                    action-content := VerticalLayout {
                        padding: Theme.inset; spacing: Theme.gap;
                        HorizontalLayout {
                            height: Theme.touch;
                            Text {
                                text: root.route == Route.worktree-actions ? "Worktrees" : root.action_renaming ? (root.action_edit_action == "new_worktree" ? "New worktree" : root.action_edit_action == "open_worktree" ? "Open worktree" : "Rename") : root.action_kind == 1 ? "Workspace actions" : root.action_kind == 2 ? "Tab actions" : "Pane controls";
                                color: Theme.ink; font-weight: 700; wrap: word-wrap; vertical-alignment: center;
                            }
                            IconButton { icon: root.route == Route.actions ? "close" : "back"; label: root.route == Route.actions ? "Close actions" : "Back"; activated => { root.management-back(); } }
                        }
                        if !root.action_renaming: Text { text: root.action_name; color: Theme.secondary; font-size: Theme.type-small; wrap: word-wrap; }
                        Rectangle {
                            visible: root.action_renaming;
                            height: root.action_renaming ? rename-content.preferred-height : 0px;
                            rename-content := VerticalLayout {
                            spacing: Theme.gap;
                            action-rename-field := ConnectionField {
                                label: root.action_edit_action == "new_worktree" ? "New branch" : root.action_edit_action == "open_worktree" ? "Existing worktree path" : root.action_kind == 1 ? "Workspace name" : root.action_kind == 2 ? "Tab name" : "Pane name";
                                enabled: root.application_active && root.action_renaming; value <=> root.action_value;


                            }
                            HorizontalLayout {
                                height: Theme.touch; spacing: Theme.gap;
                                TouchButton { label: "Cancel"; activated => { root.management-back(); } }
                                TouchButton {
                                    label: root.action_edit_action == "new_worktree" ? "Create" : root.action_edit_action == "open_worktree" ? "Open" : "Save"; active: true; enabled: root.action_value != "";
                                    activated => {
                                        let action = root.action_edit_action;
                                        let id = root.action_id; let workspace = root.action_workspace; let value = root.action_value;
                                        root.dismiss-overlays(); root.stock-action(action, id, workspace, value);
                                    }
                                }
                            }
                            if root.action_kind == 3 && root.action_custom_label: TouchButton {
                                height: Theme.touch; label: "Use automatic name";
                                activated => { let id=root.action_id; root.dismiss-overlays(); root.stock-action("clear_pane_name",id,"",""); }
                            }
                            if root.action_kind == 3 && !root.action_custom_label: Text {
                                text: "Currently using the automatic name."; color: Theme.ink; wrap: word-wrap;
                            }
                            }
                        }
                        if root.route == Route.actions && root.action_kind == 1: VerticalLayout {
                            spacing: Theme.gap;
                            TouchButton { height: Theme.touch; text-alignment: left; label: "New tab"; activated => { let workspace=root.action_workspace;root.dismiss-overlays(); root.stock-action("new_tab", "", workspace, ""); } }
                            TouchButton { height: Theme.touch; text-alignment: left; label: "Rename workspace…"; activated => { root.edit-resource("rename_workspace",root.action_name); } }
                            TouchButton { height: Theme.touch; text-alignment: left; label: "Worktrees…"; activated => { root.navigate(Route.worktree-actions); } }
                            TouchButton { height: Theme.touch; text-alignment: left; label: "Close workspace…"; activated => { root.confirm-resource("close_workspace"); } }
                        }
                        if root.route == Route.worktree-actions: VerticalLayout {
                            spacing: Theme.gap;
                            if !root.action_worktree_linked: TouchButton { height: Theme.touch; text-alignment: left; label: "New worktree…"; activated => { root.edit-resource("new_worktree",""); } }
                            if !root.action_worktree_linked: TouchButton { height: Theme.touch; text-alignment: left; label: "Open worktree…"; activated => { root.edit-resource("open_worktree",""); } }
                            if root.action_worktree_linked: TouchButton { height: Theme.touch; text-alignment: left; label: "Delete worktree checkout…"; activated => { root.confirm-resource("remove_worktree"); } }
                            if root.action_has_worktree_children: TouchButton {
                                height: Theme.touch; text-alignment: left;
                                label: root.action_group_collapsed ? "Expand group" : "Collapse group";
                                activated => { let key=root.action_worktree_key;root.dismiss-overlays();root.stock-action("toggle_group",key,"",""); }
                            }
                        }
                        if root.route == Route.actions && root.action_kind == 2: VerticalLayout {
                            spacing: Theme.gap;
                            TouchButton { height: Theme.touch; text-alignment: left; label: "New tab"; activated => { let workspace=root.focused_workspace_id;root.dismiss-overlays(); root.stock-action("new_tab", "", workspace, ""); } }
                            TouchButton { height: Theme.touch; text-alignment: left; label: "Rename tab…"; activated => { root.edit-resource("rename_tab",root.action_name); } }
                            TouchButton { height: Theme.touch; text-alignment: left; label: "Close tab…"; activated => { root.confirm-resource("close_tab"); } }
                        }
                        if root.route == Route.actions && root.action_kind == 3: VerticalLayout {
                            spacing: Theme.inset;
                            HorizontalLayout {
                                height: Theme.touch; spacing: Theme.inset;
                                TouchButton { icon: "split"; label: "Split right"; enabled: root.connected; activated => { let id=root.action_id;let workspace=root.action_workspace;root.dismiss-overlays();root.stock-action("split_right",id,workspace,""); } }
                                TouchButton { icon: "splitDown"; label: "Split down"; enabled: root.connected; activated => { let id=root.action_id;let workspace=root.action_workspace;root.dismiss-overlays();root.stock-action("split_down",id,workspace,""); } }
                                TouchButton { icon: "zoom"; label: root.action_zoomed ? "Restore" : "Zoom"; enabled: root.connected && (root.action_can_zoom || root.action_zoomed); activated => { let id=root.action_id;root.dismiss-overlays();root.stock-action("zoom_pane",id,"",""); } }
                            }
                            HorizontalLayout {
                                height: Theme.touch; spacing: Theme.inset;
                                TouchButton { icon: "rename"; label: "Rename pane"; enabled: root.connected; activated => { root.edit-resource("rename_pane",root.action_name); } }
                                TouchButton { icon: "split"; label: "Swap pane"; accessible-label: "Swap with focused pane"; enabled: root.connected && root.action_can_swap && root.focused_pane_id != "" && root.focused_pane_id != root.action_id;
                                    activated => { let id=root.action_id;let focused=root.focused_pane_id;root.dismiss-overlays();root.stock-action("swap_panes",focused,id,""); } }
                            }
                            Rectangle { height: Theme.hairline; background: Theme.separator; }
                            HorizontalLayout {
                                height: Theme.touch; spacing: Theme.inset;
                                TouchButton { icon: "trash"; label: "Close pane…"; enabled: root.connected; activated => { root.confirm-resource("close_pane"); } }
                                Rectangle { horizontal-stretch: 1; }
                            }
                        }
                    }
                }
                if root.route == Route.confirm-action: Rectangle {
                    x: (parent.width - self.width) / 2; y: Theme.inset;
                    width: min(parent.width - Theme.inset * 2, Theme.sidebar-width);
                    height: confirm-content.preferred-height;
                    background: Theme.paper; border-color: Theme.ink; border-width: Theme.rule;
                    TouchArea { }
                    confirm-content := VerticalLayout {
                        padding: Theme.inset; spacing: Theme.inset;
                        Text {
                            text: root.confirming_action == "remove_worktree" ? "Delete worktree checkout " + root.action_name + "?" : "Close " + (root.action_name == "" ? (root.action_kind == 1 ? "workspace" : root.action_kind == 2 ? "tab" : "pane") : root.action_name) + "?";
                            color: Theme.ink; font-size: Theme.type-body; font-weight: 700; wrap: word-wrap;
                        }
                        Text { text: "This may terminate running remote processes."; color: Theme.ink; wrap: word-wrap; }
                        HorizontalLayout {
                            height: Theme.touch; spacing: Theme.gap;
                            TouchButton { label: "Cancel"; activated => { root.management-back(); } }
                            TouchButton {
                                label: root.confirming_action == "remove_worktree" ? "Delete" : "Close"; active: true;
                                activated => {
                                    let action=root.confirming_action;let id=root.action_id;let workspace=root.action_workspace;
                                    root.dismiss-overlays();root.stock-action(action,id,workspace,"");
                                }
                            }
                        }
                    }
                }

                if root.menu_visible: Rectangle {
                    x: 0px; y: 0px;
                    width: parent.width;
                    height: parent.height;
                    background: Theme.paper;
                    border-width: 0px;
                    TouchArea { }
                    menu-content := VerticalLayout {
                        padding: Theme.inset; spacing: Theme.gap; alignment: start;
                        HorizontalLayout {
                            height: Theme.touch;
                            if root.route != Route.menu && root.route != Route.help: IconButton { icon: "back"; label: "Back to Menu"; activated => { root.navigate(Route.menu); } }
                            Text { text: root.route == Route.menu ? "Menu" : root.route == Route.settings ? "Settings" : root.route == Route.panes ? "Pane actions" : "Help & shortcuts"; font-weight: 700; color: Theme.ink; vertical-alignment: center; }
                            IconButton { icon: "close"; label: "Close"; activated => { root.dismiss-overlays(); } }
                        }
                        if root.route == Route.menu: VerticalLayout {
                            spacing: Theme.gap; alignment: start;
                            HorizontalLayout {
                                height: 128px; spacing: Theme.inset;
                                TouchButton { subtle: true; icon: "monitor"; label: "Manage hosts"; activated => { root.open-connections(); } }
                                TouchButton { subtle: true; icon: "terminal"; label: "Shown sessions"; activated => { root.navigate(Route.sessions); } }
                            }
                            HorizontalLayout {
                                height: 128px; spacing: Theme.inset;
                                TouchButton { subtle: true; icon: "settings"; label: "Settings"; activated => { root.navigate(Route.settings); } }
                                TouchButton { subtle: true; icon: "help"; label: "Help & shortcuts"; activated => { root.navigate(Route.help); } }
                            }
                            HorizontalLayout {
                                height: Theme.touch; spacing: Theme.inset;
                                TouchButton { icon: "quit"; label: "Quit kherdr"; activated => { root.quit(); } }
                                Rectangle { horizontal-stretch: 2; }
                            }
                            Rectangle { height: Theme.inset; }
                            Rectangle { height: Theme.hairline; background: Theme.separator; }
                            Text { text: "Current session · " + root.current_session_label; color: Theme.secondary; font-size: Theme.type-small; }
                            HorizontalLayout {
                                height: Theme.touch; spacing: Theme.inset;
                                TouchButton { icon: "split"; label: "Pane actions"; enabled: root.connected && root.focused_pane_id != ""; activated => { root.navigate(Route.panes); } }
                                TouchButton { icon: "reload"; label: "Reload config"; enabled: root.connected; activated => { root.dismiss-overlays(); root.stock-action("reload_config","","",""); } }
                                TouchButton { icon: "close"; label: "Disconnect view"; enabled: root.connected || root.connecting_or_retrying; activated => { root.dismiss-overlays(); root.disconnect(); } }
                            }
                        }
                        if root.route == Route.settings: VerticalLayout {
                            spacing: Theme.inset;
                            TouchButton { label: "Credentials"; activated => { root.open-credentials(); } }
                            TouchButton { label: root.keyboard_visible ? "Hide keyboard" : "Show keyboard"; activated => { root.set-keyboard(!root.keyboard_visible); } }
                            HorizontalLayout {
                                height: Theme.touch;
                                TouchButton { label: "Small"; active: root.text_size == 0; activated => { root.set-text-size(0); } }
                                TouchButton { label: "Medium"; active: root.text_size == 1; activated => { root.set-text-size(1); } }
                                TouchButton { label: "Large"; active: root.text_size == 2; activated => { root.set-text-size(2); } }
                            }
                            Text { text: "Terminal text preview\n$ herdr"; color: Theme.ink; font-family: root.terminal_font_family; font-size: root.terminal_font_size * 1phx; wrap: word-wrap; }
                        }
                        if root.route == Route.help: Flickable {
                            min-height: 0px;
                            vertical-stretch: 1;
                            viewport-width: self.width;
                            viewport-height: max(self.height, help-content.preferred-height);
                            help-content := VerticalLayout {
                                x: 0px; y: 0px;
                                width: parent.width; height: self.preferred-height;
                                alignment: start;
                                spacing: Theme.inset;
                                for item in [
                                    {icon: "workspaces", title: "Switch terminals", detail: "Open Machines at the top left. Expand a session and choose a workspace."},
                                    {icon: "keyboard", title: "Touch keyboard", detail: "123 opens symbols. Fn opens function keys."},
                                    {icon: "swipe", title: "Scroll", detail: "Swipe vertically. PgUp and PgDn send key presses to the terminal application."},
                                    {icon: "split", title: "Pane actions", detail: "Open Menu, then Pane actions. Tap the pane's ellipsis button."},
                                    {icon: "copy", title: "Select, copy and paste", detail: "Hold terminal text, then drag. Copy and Paste appear with the selection."}
                                ]: HorizontalLayout {
                                    min-height: 112px; spacing: Theme.inset;
                                    Glyph { name: item.icon; }
                                    VerticalLayout {
                                        spacing: Theme.gap; alignment: start;
                                        Text { text: item.title; color: Theme.ink; font-weight: 700; }
                                        Text { text: item.detail; color: Theme.secondary; font-size: Theme.type-small; wrap: word-wrap; }
                                    }
                                }
                                Rectangle { height: Theme.hairline; background: Theme.separator; }
                                Text { text: "kherdr by theblazehen · Herdr 0.9"; color: Theme.secondary; font-size: Theme.type-small; }
                                Text { text: "Tap the clock to open Kindle Quick Settings and adjust brightness. Quit kherdr leaves Herdr sessions running."; color: Theme.secondary; font-size: Theme.type-small; wrap: word-wrap; }
                                if root.capability_status != "": Text { text: root.capability_status; color: Theme.secondary; font-size: Theme.type-small; wrap: word-wrap; }
                            }
                        }
                        if root.route == Route.panes: Flickable {
                            min-height: 0px; vertical-stretch: 1; viewport-width: self.width;
                            viewport-height: max(self.height, pane-content.preferred-height);
                            pane-content := VerticalLayout {
                                width: parent.width; height: self.preferred-height; spacing: Theme.gap;
                                for pane in root.pane_rows: PaneRow {
                                    entry: pane;
                                    activated => { root.dismiss-overlays(); root.select-pane(pane.pane_id); }
                                    menu-requested => { root.open-pane-actions(pane); }
                                }
                            }
                        }
                    }
                }
                Rectangle {
                    visible: (root.connections_visible || root.editing_connection) && !root.credentials-visible && !root.auth_prompt_visible;
                    width: parent.width; height: parent.height;
                    background: Theme.paper;
                    TouchArea { }
                    VerticalLayout {
                        padding: Theme.inset; spacing: Theme.gap;
                        if root.connection_error != "": Text { text: root.connection_error; color: Theme.ink; wrap: word-wrap; }
                        if !root.editing_connection: Flickable {
                            min-height: 0px; vertical-stretch: 1; viewport-width: self.width;
                            viewport-height: max(self.height, connections-content.preferred-height);
                            connections-content := VerticalLayout {
                                x: 0px; y: 0px; width: parent.width; height: self.preferred-height;
                                spacing: Theme.inset; alignment: start;
                                if root.route == Route.entry: VerticalLayout {
                                    spacing: Theme.inset; alignment: start;
                                    TouchButton { height: 112px; icon: "kindle"; label: "Local terminal"; text-alignment: left; activated => { root.open-host("@local"); } }
                                    TouchButton { height: 112px; icon: "monitor"; label: "Connect via SSH"; text-alignment: left;
                                        activated => { if root.host_entries.length <= 1 { root.new-connection(); } else { root.open-connections(); } } }
                                }
                                for entry in root.host_entries: VerticalLayout {
                                    alignment: start;
                                    if root.route == Route.hosts && root.host_detail_id == "": TouchButton {
                                        height: Theme.touch; text-alignment: left;
                                        icon: entry.local ? "kindle" : "monitor";
                                        label: entry.local ? "Local terminal" : entry.name + (entry.name == entry.endpoint ? "" : "\n" + entry.endpoint);
                                        accessible-label: entry.local ? "Local terminal" : "Sessions on " + entry.name + ", " + entry.endpoint;
                                        activated => { root.open-host(entry.id); }
                                    }
                                }
                                if !root.sessions_visible && root.host_detail_id != "": VerticalLayout {
                                    spacing: Theme.inset; alignment: start;
                                    if root.host_detail.endpoint != root.host_detail.name: Text {
                                        text: root.host_detail.endpoint; color: Theme.ink; wrap: word-wrap;
                                    }
                                    if root.setup_busy || root.setup_inspected: HorizontalLayout {
                                        height: Theme.touch; spacing: Theme.gap;
                                        Text { text: root.setup_busy ? "Finding sessions…" : "Sessions"; color: Theme.ink; font-weight: 700; vertical-alignment: center; horizontal-stretch: 1; }
                                        TouchButton {
                                            width: Theme.touch * 2;
                                            label: root.setup_busy ? "Cancel" : "Refresh";
                                            activated => { if root.setup_busy { root.cancel-setup(); } else { root.inspect-setup(); } }
                                        }
                                    }
                                    if !root.setup_busy && !root.setup_inspected: VerticalLayout {
                                        spacing: Theme.inset; alignment: start;
                                        Text { text: "Could not load sessions"; color: Theme.ink; font-weight: 700; wrap: word-wrap; }
                                        Text { text: root.setup_detail; color: Theme.ink; wrap: word-wrap; }
                                        HorizontalLayout {
                                            height: Theme.touch; spacing: Theme.inset;
                                            TouchButton { width: Theme.touch * 2; label: "Retry"; active: true; activated => { root.inspect-setup(); } }
                                            TouchButton { width: Theme.touch * 3; label: "Edit connection"; activated => { root.edit-connection(root.host_detail_id); } }
                                            Rectangle { horizontal-stretch: 1; }
                                        }
                                    }
                                    if !root.setup_busy && root.setup_inspected && root.setup_available && root.setup_sessions.length == 0: VerticalLayout {
                                        spacing: Theme.inset; alignment: start;
                                        Text { text: "No running Herdr sessions"; color: Theme.ink; font-weight: 700; wrap: word-wrap; }
                                        Text { text: "Start a Herdr session, or open an SSH terminal on this host."; color: Theme.ink; wrap: word-wrap; }
                                        HorizontalLayout {
                                            height: Theme.touch; spacing: Theme.inset;
                                            TouchButton { width: Theme.touch * 3; label: "New session…"; activated => { root.new-herdr-session(root.host_detail_id); } }
                                            TouchButton { width: Theme.touch * 3; label: "Open SSH terminal"; activated => { root.open-shell(root.host_detail_id); } }
                                            Rectangle { horizontal-stretch: 1; }
                                        }
                                    }
                                    for session in root.host_sessions: VerticalLayout {
                                        spacing: Theme.unit; alignment: start;
                                        HorizontalLayout {
                                            height: Theme.touch; spacing: Theme.gap;
                                            SessionCheck {
                                                checked: session.checked;
                                                enabled: root.auth-controls && (session.checked || (session.available && !root.setup_busy));
                                                label: "Show " + session.name + " on this Kindle";
                                                toggled => { root.set-session-visible(root.host_detail_id, session.name, !session.checked); }
                                            }
                                            Text { text: session.name; color: Theme.ink; font-weight: 700; vertical-alignment: center; overflow: elide; }
                                            Text { width: Theme.touch * 2.5; text: session.state; color: Theme.secondary; font-size: Theme.type-small; vertical-alignment: center; wrap: word-wrap; }
                                            TouchButton { width: Theme.touch * 2; icon: session.can_retry ? "reload" : "play"; label: session.can_retry ? "Retry" : "Open";
                                                enabled: root.auth-controls && (session.checked || (session.available && !root.setup_busy));
                                                accessible-label: "Open " + session.name + " on " + root.host_detail.name;
                                                activated => { root.open-host-session(root.host_detail_id, session.name); } }
                                        }
                                        if session.detail != "": Text { text: session.detail; color: Theme.ink; wrap: word-wrap; }
                                    }
                                    if root.host_sessions.length > 0: VerticalLayout {
                                        spacing: Theme.inset; alignment: start;
                                        Text { text: "Checked sessions appear on this Kindle. Unchecking leaves remote work running."; color: Theme.secondary; font-size: Theme.type-small; wrap: word-wrap; }
                                        HorizontalLayout { height: Theme.touch; spacing: Theme.inset;
                                            TouchButton { icon: "plus"; label: "New session…"; enabled: !root.setup_busy; activated => { root.new-herdr-session(root.host_detail_id); } }
                                            TouchButton { icon: "terminal"; label: "SSH terminal"; enabled: !root.setup_busy; activated => { root.open-shell(root.host_detail_id); } }
                                        }
                                        HorizontalLayout { height: Theme.touch; spacing: Theme.inset;
                                            TouchButton { icon: "settings"; label: "Host settings"; activated => { root.edit-connection(root.host_detail_id); } }
                                            TouchButton { icon: "reload"; label: "Check again"; enabled: !root.setup_busy; activated => { root.inspect-setup(); } }
                                        }
                                    }
                                }
                                if root.sessions_visible && root.open_sessions.length == 0: Text {
                                    text: "No sessions are shown here. Choose a host to find its running sessions."; color: Theme.ink; wrap: word-wrap;
                                }
                                for session in root.open_sessions: VerticalLayout {
                                    alignment: start;
                                    if root.sessions_visible: HorizontalLayout {
                                        height: Theme.touch; spacing: Theme.inset;
                                        TouchButton {
                                            text-alignment: left; active: session.selected;
                                            label: session.host_name + " · " + session.name + "\n" + session.kind + " · " + session.state;
                                            accessible-label: "View " + session.name + " on " + session.host_name;
                                            activated => { root.resume-session(session.id); }
                                        }
                                        TouchButton {
                                            width: Theme.touch * 3;
                                            label: session.kind == "SSH in Local" ? "Close SSH pane…" : session.host_id == "@local" ? "Disconnect Local view" : "Hide from device";
                                            activated => { if session.kind == "SSH in Local" { root.closing-ssh = session.id; } else { root.close-session(session.id); } }
                                        }
                                    }
                                }
                            }
                        }
                        editor-scroll := Flickable {
                            visible: root.editing_connection;
                            min-height: 0px; preferred-height: 0px;
                            max-height: root.editing_connection ? 32767px : 0px;
                            vertical-stretch: root.editing_connection ? 1 : 0;
                            viewport-width: self.width;
                            viewport-height: max(self.height, editor-content.preferred-height);
                            editor-content := VerticalLayout {
                                width: parent.width; height: self.preferred-height; spacing: Theme.gap;
                                host-field := ConnectionField {
                                    label: "Host"; value <=> root.connection_draft.host;
                                    enabled: root.application_active && root.editing_connection;

                                    focused => {
                                        root.field-focused(1);
                                        if host-field.y + editor-scroll.viewport-y < 0px { editor-scroll.viewport-y = -host-field.y; }
                                        else if host-field.y + host-field.height + editor-scroll.viewport-y > editor-scroll.height {
                                            editor-scroll.viewport-y = min(0px, editor-scroll.height - host-field.y - host-field.height);
                                        }
                                    }
                                }
                                user-field := ConnectionField {
                                    label: "User"; value <=> root.connection_draft.user;
                                    enabled: root.application_active && root.editing_connection;

                                    focused => {
                                        root.field-focused(2);
                                        if user-field.y + editor-scroll.viewport-y < 0px { editor-scroll.viewport-y = -user-field.y; }
                                        else if user-field.y + user-field.height + editor-scroll.viewport-y > editor-scroll.height {
                                            editor-scroll.viewport-y = min(0px, editor-scroll.height - user-field.y - user-field.height);
                                        }
                                    }
                                }
                                HorizontalLayout {
                                    height: Theme.touch; spacing: Theme.inset;
                                    Text { width: min(176px, root.width * 0.28); text: "Sign in with"; color: Theme.ink; font-size: Theme.type-small; vertical-alignment: center; }
                                    TouchButton { width: min(Theme.touch * 2.5, (parent.width - min(176px, root.width * 0.28) - Theme.inset * 3) / 2); label: "Password"; active: root.connection_draft.auth_method == "password"; activated => { root.connection_draft.auth_method = "password"; } }
                                    TouchButton { width: min(Theme.touch * 2.5, (parent.width - min(176px, root.width * 0.28) - Theme.inset * 3) / 2); label: "SSH key"; active: root.connection_draft.auth_method != "password"; activated => { root.connection_draft.auth_method = "key"; } }
                                    Rectangle { horizontal-stretch: 1; }
                                }
                                key-picker := TouchButton {
                                    label: root.selected_key_name == "" ? "Choose SSH key" : "Key: " + root.selected_key_name;
                                    visible: root.connection_draft.auth_method != "password";
                                    height: self.visible ? Theme.touch : 0px;
                                    activated => { root.open-key-picker(); }
                                }
                                TouchButton { icon: root.advanced ? "down" : "chevron"; plain: true; label: "Advanced"; enabled: root.editing_connection;
                                    activated => { root.advanced = !root.advanced; if !root.advanced { root.editor-field = 2; user-field.focus(); } } }
                                advanced-content := Rectangle {
                                    visible: root.advanced;
                                    height: root.advanced ? advanced-fields.preferred-height : 0px;
                                    clip: true;
                                    advanced-fields := VerticalLayout {
                                        width: parent.width;
                                        height: self.preferred-height;
                                        spacing: Theme.gap;
                                name-field := ConnectionField {
                                    label: "Name"; value <=> root.connection_draft.name;
                                    enabled: root.application_active && root.editing_connection && root.advanced;

                                    focused => {
                                        root.field-focused(0);
                                        if advanced-content.y + name-field.y + editor-scroll.viewport-y < 0px { editor-scroll.viewport-y = -(advanced-content.y + name-field.y); }
                                        else if advanced-content.y + name-field.y + name-field.height + editor-scroll.viewport-y > editor-scroll.height {
                                            editor-scroll.viewport-y = min(0px, editor-scroll.height - advanced-content.y - name-field.y - name-field.height);
                                        }
                                    }
                                }
                                port-field := ConnectionField {
                                    label: "Port"; value <=> root.connection_draft.port;
                                    enabled: root.application_active && root.editing_connection && root.advanced;

                                    focused => {
                                        root.field-focused(3);
                                        if port-field.y + editor-scroll.viewport-y < 0px { editor-scroll.viewport-y = -port-field.y; }
                                        else if port-field.y + port-field.height + editor-scroll.viewport-y > editor-scroll.height {
                                            editor-scroll.viewport-y = min(0px, editor-scroll.height - port-field.y - port-field.height);
                                        }
                                    }
                                }
                                herdr-session-field := ConnectionField {
                                    label: "Default Herdr session"; value <=> root.connection_draft.herdr_session;
                                    enabled: root.application_active && root.editing_connection && root.advanced;
                                    focused => { root.field-focused(6); }

                                }
                                herdr-binary-field := ConnectionField {
                                    label: "Herdr binary"; value <=> root.connection_draft.herdr_binary;
                                    enabled: root.application_active && root.editing_connection && root.advanced;
                                    focused => { root.field-focused(7); }

                                }
                                keepalive-field := ConnectionField {
                                    label: "Keepalive seconds"; value <=> root.connection_draft.keepalive;
                                    enabled: root.application_active && root.editing_connection && root.advanced;

                                    focused => {
                                        root.field-focused(8);
                                        if (advanced-content.y + keepalive-field.y) + editor-scroll.viewport-y < 0px { editor-scroll.viewport-y = -(advanced-content.y + keepalive-field.y); }
                                        else if (advanced-content.y + keepalive-field.y) + keepalive-field.height + editor-scroll.viewport-y > editor-scroll.height {
                                            editor-scroll.viewport-y = min(0px, editor-scroll.height - (advanced-content.y + keepalive-field.y) - keepalive-field.height);
                                        }
                                    }
                                }
                                    TouchButton { label: root.connection_draft.compression ? "Compression: on" : "Compression: off"; enabled: root.editing_connection && root.advanced; active: root.connection_draft.compression; activated => { root.connection_draft.compression = !root.connection_draft.compression; root.focus-editor(); } }
                                    }
                                }
                                HorizontalLayout {
                                    height: Theme.touch; spacing: Theme.inset;
                                    TouchButton { width: Theme.touch * 3; icon: "play"; label: root.connection_draft.id == "" ? "Connect" : "Save settings"; active: true;
                                        enabled: root.connection_draft.host != ""; activated => { root.save-connection(root.connection_draft); } }
                                    Rectangle { horizontal-stretch: 1; }
                                }
                            }
                        }
                        if root.editing_connection: HorizontalLayout {
                            height: Theme.touch; spacing: Theme.gap;
                            TouchButton { width: Theme.touch * 2; label: "Copy"; enabled: root.local_copy_allowed; activated => { root.copy(); } }
                            TouchButton { width: Theme.touch * 2; label: "Paste"; enabled: root.local_editor_active; activated => { root.paste(); } }
                            Rectangle { horizontal-stretch: 1; }
                        }
                    }
                    if root.route == Route.host-actions: TouchArea {
                        width: parent.width; height: parent.height;
                        clicked => { root.management-back(); }
                    }
                    if root.route == Route.host-actions: Rectangle {
                        x: parent.width - self.width - Theme.inset; y: Theme.gap;
                        width: min(parent.width - Theme.inset * 2, Theme.sidebar-width);
                        height: min(parent.height - Theme.inset * 2, host-actions-content.preferred-height);
                        background: Theme.paper; border-color: Theme.ink; border-width: Theme.rule;
                        TouchArea { }
                        Flickable {
                            width: parent.width; height: parent.height; viewport-width: self.width;
                            viewport-height: max(self.height, host-actions-content.preferred-height);
                            host-actions-content := VerticalLayout {
                                x: 0px; y: 0px; width: parent.width; height: self.preferred-height;
                                padding: Theme.gap; spacing: Theme.gap; alignment: start;
                                if root.host_detail_id == "": VerticalLayout {
                                    spacing: Theme.gap; alignment: start;
                                    TouchButton { height: Theme.touch; text-alignment: left; label: "Shown on this device"; activated => { root.navigate(Route.sessions); } }
                                    TouchButton { height: Theme.touch; text-alignment: left; label: "Credentials"; activated => { root.navigate(Route.hosts); root.open-credentials(); } }
                                    TouchButton { height: Theme.touch; text-alignment: left; label: "Quit kherdr"; activated => { root.quit(); } }
                                }
                                if root.host_detail_id != "": VerticalLayout {
                                    spacing: Theme.gap; alignment: start;
                                    TouchButton { height: Theme.touch; text-alignment: left; label: "Connection settings"; activated => { root.edit-connection(root.host_detail_id); } }
                                    TouchButton { height: Theme.touch; text-alignment: left; label: "New Herdr session…"; activated => { root.new-herdr-session(root.host_detail_id); } }
                                    TouchButton { height: Theme.touch; text-alignment: left; label: "Open SSH terminal"; activated => { root.open-shell(root.host_detail_id); } }
                                    TouchButton { height: Theme.touch; text-alignment: left; label: "Inspect Herdr…"; activated => { root.setup-herdr(root.host_detail_id); } }
                                    if root.host_detail.open: TouchButton {
                                        height: Theme.touch;
                                        text-alignment: left; label: "Hide all sessions from this Kindle";
                                        activated => { root.navigate(Route.hosts); root.disconnect-host(root.host_detail_id); }
                                    }
                                    TouchButton {
                                        height: Theme.touch;
                                        text-alignment: left; label: "Forget saved host…"; enabled: !root.setup_busy;
                                        activated => { root.removing-name = root.host_detail.name; root.removing-connection = root.host_detail_id; }
                                    }
                                }
                            }
                        }
                    }
                    if root.closing-ssh != "": Rectangle {
                        width: parent.width; height: parent.height; background: Theme.paper;
                        TouchArea { }
                        VerticalLayout {
                            padding: Theme.inset; spacing: Theme.inset; alignment: start;
                            Text { text: "Close SSH pane?"; color: Theme.ink; font-weight: 700; }
                            Text { text: "This closes the SSH connection and may terminate its remote commands. To leave it running, return to the terminal or quit kherdr instead."; color: Theme.ink; wrap: word-wrap; }
                            HorizontalLayout {
                                height: Theme.touch;
                                TouchButton { label: "Cancel"; activated => { root.closing-ssh = ""; } }
                                TouchButton { label: "Close SSH pane"; activated => { let id = root.closing-ssh; root.closing-ssh = ""; root.close-session(id); } }
                            }
                        }
                    }
                    if root.removing-connection != "": Rectangle {
                        width: parent.width; height: parent.height; background: Theme.paper;
                        init => { remove-cancel.focus(); }
                        TouchArea { }
                        VerticalLayout {
                            padding: Theme.inset; spacing: Theme.inset; alignment: start;
                            Text { text: "Remove " + root.removing-name + "?"; color: Theme.ink; font-weight: 700; wrap: word-wrap; }
                            Text { text: "Removes this saved connection only. Remote workspaces and processes are not closed."; color: Theme.ink; wrap: word-wrap; }
                            HorizontalLayout {
                                height: Theme.touch;
                                remove-cancel := TouchButton { label: "Cancel"; activated => { root.removing-connection = ""; manager-close.focus(); } }
                                TouchButton { label: "Remove connection"; activated => { let id = root.removing-connection; root.removing-connection = ""; root.remove-connection(id); } }
                            }
                        }
                    }
                }
                // Auth panels sit above connection management and never cover the keyboard.
                Rectangle {
                    visible: root.credentials-visible && !root.auth_prompt_visible;
                    width: parent.width; height: parent.height; background: Theme.paper;
                    TouchArea { }
                    VerticalLayout {
                        padding: Theme.inset; spacing: Theme.gap;
                        if root.route == Route.credentials: VerticalLayout {
                            vertical-stretch: 1; spacing: Theme.inset; alignment: start;
                            TouchButton { label: "SSH keys"; enabled: root.auth-controls; activated => { root.navigate(Route.keys); } }
                            TouchButton { label: "Trusted server identities"; enabled: root.auth-controls; activated => { root.open-trusted-hosts(); } }
                            Text { text: "Saved passwords are stored as plaintext on this Kindle."; color: Theme.ink; wrap: word-wrap; }
                            TouchButton { label: "Forget saved passwords…"; enabled: root.auth-controls; activated => { root.set-auth-removing("@saved-passwords"); auth-remove-cancel.focus(); } }
                        }
                        if root.auth_error != "": Text { text: root.auth_error; color: Theme.ink; wrap: word-wrap; }
                        if root.auth_busy: Text { text: "Updating key library…"; color: Theme.ink; wrap: word-wrap; }
                        if root.route == Route.keys || root.route == Route.key-picker: Flickable {
                            min-height: 0px; vertical-stretch: 1;
                            viewport-width: self.width; viewport-height: max(self.height, key-list.preferred-height);
                            key-list := VerticalLayout {
                                x: 0px; y: 0px;
                                width: parent.width; height: self.preferred-height; spacing: Theme.inset; alignment: start;
                                if root.key_entries.length == 0: Text { text: "No keys yet. Generate a new key or import one you already use."; color: Theme.ink; wrap: word-wrap; }
                                for entry in root.key_entries: VerticalLayout {
                                    height: self.preferred-height; spacing: Theme.gap;
                                    Text { text: entry.name; color: Theme.ink; font-weight: 700; wrap: word-wrap; }
                                    Text { text: entry.detail; color: Theme.ink; wrap: word-wrap; }
                                    HorizontalLayout {
                                        height: Theme.touch;
                                        if root.route == Route.key-picker: TouchButton { label: "Use key"; enabled: root.auth-controls; activated => { root.choose-key(entry.path); } }
                                        if root.route == Route.keys: TouchButton { label: "Details"; enabled: root.auth-controls; activated => { root.view-key(entry.id); } }
                                        if root.route == Route.keys: TouchButton { label: "Rename"; enabled: root.auth-controls; activated => { root.edit-key(entry.id); } }
                                    }
                                }
                            }
                        }
                        key-editor-scroll := Flickable {
                            visible: root.route == Route.key-editor;
                            min-height: 0px; preferred-height: 0px; max-height: root.route == Route.key-editor ? 32767px : 0px;
                            vertical-stretch: root.route == Route.key-editor ? 1 : 0;
                            viewport-width: self.width; viewport-height: max(self.height, key-editor-content.preferred-height);
                            key-editor-content := VerticalLayout {
                                x: 0px; y: 0px;
                                width: parent.width; height: self.preferred-height; spacing: Theme.gap;
                                key-name-field := ConnectionField {
                                    label: "Key name"; value <=> root.key_name; enabled: root.auth-controls && root.route == Route.key-editor;
                                    focused => { root.auth-field-focused(0, self.y, self.height); }

                                }
                                if root.key_id == "": HorizontalLayout {
                                    height: Theme.touch;
                                    TouchButton { label: "Generate"; active: root.key_source == "generate"; enabled: root.auth-controls; activated => { root.change-key-source("generate"); } }
                                    TouchButton { label: "Import file"; active: root.key_source == "file"; enabled: root.auth-controls; activated => { root.change-key-source("file"); } }
                                    TouchButton { label: "Paste key"; active: root.key_source == "paste"; enabled: root.auth-controls; activated => { root.change-key-source("paste"); } }
                                }
                                if root.key_id == "" && root.key_source == "generate": Text { text: "Creates an Ed25519 key. Copy its public key to your server after saving."; color: Theme.ink; wrap: word-wrap; }
                                key-path-field := ConnectionField {
                                    visible: root.key_id == "" && root.key_source == "file";
                                    height: self.visible ? Theme.touch + Theme.type-small + Theme.gap : 0px;
                                    label: "Private key file"; value <=> root.key_path; enabled: root.auth-controls && root.route == Route.key-editor && self.visible;
                                    focused => { root.auth-field-focused(1, self.y, self.height); }

                                }
                                if root.key_id == "" && root.key_source == "file": TouchButton { label: "Browse files"; enabled: root.auth-controls; activated => { root.browse-key-files(root.key_directory); } }
                                key-material-field := ConnectionField {
                                    visible: root.key_id == "" && root.key_source == "paste";
                                    height: self.visible ? Theme.touch * 2 + Theme.type-small + Theme.gap : 0px;
                                    label: "Private key (paste complete contents)"; value <=> root.key_material; secret: true; multiline: true;
                                    enabled: root.auth-controls && root.route == Route.key-editor && self.visible;
                                    focused => { root.auth-field-focused(2, self.y, self.height); }

                                }
                                key-passphrase-field := ConnectionField {
                                    visible: root.key_id == "";
                                    height: self.visible ? Theme.touch + Theme.type-small + Theme.gap : 0px;
                                    label: root.key_source == "generate" ? "Protect with passphrase (optional)" : "Existing key passphrase (if encrypted)";
                                    value <=> root.key_passphrase; secret: true; enabled: root.auth-controls && root.route == Route.key-editor && self.visible;
                                    focused => { root.auth-field-focused(3, self.y, self.height); }

                                }
                                if root.key_id == "": Text { text: "Passphrases are not saved."; color: Theme.ink; wrap: word-wrap; }
                            }
                        }
                        if root.route == Route.trusted-hosts: Flickable {
                            min-height: 0px; vertical-stretch: 1;
                            viewport-width: self.width; viewport-height: max(self.height, trust-list.preferred-height);
                            trust-list := VerticalLayout {
                                x: 0px; y: 0px;
                                width: parent.width; height: self.preferred-height; spacing: Theme.inset; alignment: start;
                                if root.trusted_hosts.length == 0: Text { text: "No trusted hosts yet. Your first connection asks you to verify and save the server fingerprint."; color: Theme.ink; wrap: word-wrap; }
                                for host in root.trusted_hosts: VerticalLayout {
                                    height: self.preferred-height; spacing: Theme.gap;
                                    Text { text: host.host; color: Theme.ink; font-weight: 700; wrap: word-wrap; }
                                    Text { text: host.algorithm + " · " + host.fingerprint; color: Theme.ink; wrap: word-wrap; }
                                    TouchButton { label: "Forget host key"; enabled: root.auth-controls; activated => { root.auth-removing-host = true; root.auth-removing-name = host.host; root.set-auth-removing(host.id); auth-remove-cancel.focus(); } }
                                }
                            }
                        }
                        Rectangle {
                            visible: root.route == Route.key-files; min-height: 0px; preferred-height: 0px;
                            max-height: root.route == Route.key-files ? 32767px : 0px; vertical-stretch: root.route == Route.key-files ? 1 : 0;
                            VerticalLayout {
                                spacing: Theme.gap;
                                directory-field := ConnectionField {
                                    label: "Directory"; value <=> root.key_directory; enabled: root.auth-controls && root.route == Route.key-files;


                                }
                                HorizontalLayout {
                                    height: Theme.touch;
                                    TouchButton { label: "Open directory"; enabled: root.auth-controls; activated => { root.browse-key-files(root.key_directory); } }
                                    TouchButton { label: "Parent directory"; enabled: root.auth-controls; activated => { root.browse-key-files(root.key_directory + "/.."); } }
                                }
                                Flickable {
                                    min-height: 0px; vertical-stretch: 1; viewport-width: self.width; viewport-height: max(self.height, file-list.preferred-height);
                                    file-list := VerticalLayout {
                                        x: 0px; y: 0px;
                                        width: parent.width; height: self.preferred-height; spacing: Theme.gap; alignment: start;
                                        if root.key_files.length == 0: Text { text: "No files here. Open another directory or enter a private key path in the editor."; color: Theme.ink; wrap: word-wrap; }
                                        for file in root.key_files: TouchButton {
                                            label: (file.directory ? "Directory · " : "File · ") + file.name; enabled: root.auth-controls;
                                            activated => { if file.directory { root.browse-key-files(file.path); } else { root.choose-key-file(file.path); } }
                                        }
                                    }
                                }
                            }
                        }
                        if root.route == Route.key-details: Flickable {
                            min-height: 0px; vertical-stretch: 1; viewport-width: self.width; viewport-height: max(self.height, key-details.preferred-height);
                            key-details := VerticalLayout {
                                x: 0px; y: 0px;
                                width: parent.width; height: self.preferred-height; spacing: Theme.inset; alignment: start;
                                Text { text: root.key_name; color: Theme.ink; font-weight: 700; wrap: word-wrap; }
                                Text { text: "Fingerprint"; color: Theme.ink; font-weight: 700; }
                                Text { text: root.key_fingerprint; color: Theme.ink; wrap: word-wrap; }
                                Text { text: "Public key · safe to share"; color: Theme.ink; font-weight: 700; }
                                Text { text: root.key_public_key; color: Theme.ink; wrap: word-wrap; }
                                Text { text: "Private file: " + root.key_path; color: Theme.ink; wrap: word-wrap; }
                                TouchButton { label: "Copy public key"; enabled: root.auth-controls && root.key_public_key != ""; activated => { root.copy-public-key(); } }
                                HorizontalLayout {
                                    height: Theme.touch;
                                    TouchButton { label: "Rename"; enabled: root.auth-controls; activated => { root.edit-key(root.key_id); } }
                                    TouchButton { label: "Remove key"; enabled: root.auth-controls; activated => { root.auth-removing-host = false; root.auth-removing-name = root.key_name; root.set-auth-removing(root.key_id); auth-remove-cancel.focus(); } }
                                }
                            }
                        }
                        HorizontalLayout {
                            height: Theme.touch; spacing: Theme.gap;
                            if root.route == Route.keys || root.route == Route.key-picker: TouchButton { label: "Create key"; active: true; enabled: root.auth-controls; activated => { root.create-key(); } }
                            if root.route == Route.keys || root.route == Route.key-picker: TouchButton { label: "Import key"; enabled: root.auth-controls; activated => { root.create-key(); root.change-key-source("file"); } }
                            if root.route == Route.key-editor || root.route == Route.key-files: TouchButton { label: "Copy"; enabled: root.local_copy_allowed; activated => { root.copy(); } }
                            if root.route == Route.key-editor || root.route == Route.key-files: TouchButton { label: "Paste"; enabled: root.local_editor_active; activated => { root.paste(); } }
                            if root.route == Route.key-editor: TouchButton { label: root.key_id != "" ? "Save name" : root.key_source == "generate" ? "Generate key" : "Import key"; active: true; enabled: root.auth-controls && root.key_name != ""; activated => { root.save-key(); } }
                        }
                    }
                    Rectangle {
                        visible: root.auth-removing != ""; width: parent.width; height: parent.height; background: Theme.paper;
                        TouchArea { }
                        VerticalLayout {
                            padding: Theme.inset; spacing: Theme.inset; alignment: start;
                            Text { text: root.auth-removing == "@saved-passwords" ? "Forget all saved SSH passwords?" : (root.auth-removing-host ? "Forget host key for " : "Remove ") + root.auth-removing-name + "?"; color: Theme.ink; font-weight: 700; wrap: word-wrap; }
                            Text { text: root.auth-removing == "@saved-passwords" ? "Future sign-ins will ask for a password. Current sessions stay connected." : root.auth-removing-host ? "You will need to verify this server's fingerprint on the next connection." : "This deletes the managed private key file. Imported originals and server authorization stay unchanged."; color: Theme.ink; wrap: word-wrap; }
                            HorizontalLayout {
                                height: Theme.touch;
                                auth-remove-cancel := TouchButton { label: "Cancel"; enabled: root.application_active; activated => { root.set-auth-removing(""); auth-close.focus(); } }
                                TouchButton { label: root.auth-removing == "@saved-passwords" ? "Forget passwords" : root.auth-removing-host ? "Forget host key" : "Remove key"; enabled: root.application_active && !root.auth_busy; activated => { let id = root.auth-removing; root.set-auth-removing(""); if id == "@saved-passwords" { root.forget-saved-passwords(); } else if root.auth-removing-host { root.remove-trusted-host(id); } else { root.delete-key(id); } } }
                            }
                        }
                    }
                }
                Rectangle {
                    visible: root.setup_visible; width: parent.width; height: parent.height; background: Theme.paper;
                    TouchArea { }
                    VerticalLayout {
                        padding: Theme.inset; spacing: Theme.inset; alignment: start;
                        Text { text: root.setup_busy ? "Checking Herdr…" : root.setup_available ? "Herdr sessions" : "Herdr not detected"; color: Theme.ink; font-weight: 700; }
                        Text { text: root.setup_busy ? "Checking " + root.setup_binary + " on " + root.setup_host_name : root.setup_detail; color: Theme.secondary; font-size: Theme.type-small; wrap: word-wrap; }
                        if !root.setup_confirmation: VerticalLayout {
                            spacing: Theme.inset; alignment: start;
                            HorizontalLayout {
                                height: Theme.touch; spacing: Theme.inset;
                                TouchButton { width: Theme.touch * 3; icon: "reload"; label: "Check again"; enabled: root.application_active && !root.setup_busy; activated => { root.inspect-setup(); } }
                                TouchButton { width: Theme.touch * 4; icon: "settings"; label: "Connection settings"; enabled: root.application_active && !root.setup_busy; activated => { root.edit-connection(root.setup_host_id); } }
                                Rectangle { horizontal-stretch: 1; }
                            }
                            TouchButton { icon: "terminal"; label: "Open SSH terminal"; text-alignment: left; enabled: root.application_active && !root.setup_busy; activated => { root.open-shell(root.setup_host_id); } }
                            if root.setup_available: TouchButton { icon: "plus"; label: "Choose session name…"; enabled: root.application_active && !root.setup_busy; activated => { root.session_name_return = Route.setup; root.new_session_name = ""; root.navigate(Route.session-name); } }
                            if root.setup_start_allowed: TouchButton { icon: "play"; label: "Start '" + root.setup_session_name + "'…"; text-alignment: left; enabled: root.application_active && !root.setup_busy; activated => { root.setup_confirmation = true; } }
                        }
                        if root.setup_confirmation: VerticalLayout {
                            spacing: Theme.inset; alignment: start;
                            Text { text: "Start session '" + root.setup_session_name + "' on " + root.setup_host_name + "? Herdr may restore saved workspaces and agents."; color: Theme.ink; font-weight: 700; wrap: word-wrap; }
                            HorizontalLayout {
                                height: Theme.touch; spacing: Theme.gap;
                                TouchButton { label: "Cancel"; enabled: root.application_active && !root.setup_busy; activated => { root.setup_confirmation = false; } }
                                TouchButton { label: "Start session"; enabled: root.application_active && !root.setup_busy && root.setup_start_allowed; activated => { root.setup_confirmation = false; root.confirm-setup(); } }
                            }
                        }
                    }
                }
                Rectangle {
                    visible: root.route == Route.session-name;
                    width: parent.width; height: parent.height; background: Theme.paper;
                    TouchArea { }
                    VerticalLayout {
                        padding: Theme.inset; spacing: Theme.inset; alignment: start;
                        new-session-field := ConnectionField { label: "Session name"; value <=> root.new_session_name; enabled: root.route == Route.session-name && root.application_active; }
                        if root.connection_error != "": Text { text: root.connection_error; color: Theme.ink; wrap: word-wrap; }
                        HorizontalLayout {
                            height: Theme.touch; spacing: Theme.gap;
                            TouchButton { width: Theme.touch * 3; label: "Check name"; active: true; enabled: root.new_session_name != ""; activated => { root.inspect-session-name(root.new_session_name); } }
                            Rectangle { horizontal-stretch: 1; }
                        }
                    }
                }
                Rectangle {
                    visible: root.auth_prompt_visible; width: parent.width; height: parent.height; background: Theme.paper;
                    TouchArea { }
                    VerticalLayout {
                        x: 0px; y: 0px; width: parent.width; height: parent.height;
                        padding: Theme.inset; spacing: Theme.inset;
                        Text { text: root.auth_prompt_kind == 1 ? "Server identity changed · verify replacement" : root.auth_prompt_title; color: Theme.ink; font-size: Theme.type-body; font-weight: 700; wrap: word-wrap; }
                        prompt-scroll := Flickable {
                            min-height: 0px; vertical-stretch: 1; viewport-width: self.width; viewport-height: max(self.height, prompt-content.preferred-height);
                            prompt-content := VerticalLayout {
                                x: 0px; y: 0px;
                                width: parent.width; height: self.preferred-height; spacing: Theme.inset; alignment: start;
                                Text { text: root.auth_prompt_detail; color: Theme.secondary; font-size: Theme.type-small; wrap: word-wrap; }
                                if root.auth_prompt_kind == 0: Text { text: "First connection · compare this fingerprint with a trusted source."; color: Theme.secondary; font-size: Theme.type-small; wrap: word-wrap; }
                                if root.auth_prompt_kind == 1: Text { text: "Stop unless you expected this change. Verify the replacement through a separate trusted channel."; color: Theme.ink; font-weight: 700; wrap: word-wrap; }
                                if root.auth_prompt_kind == 1: Text { text: "Previously trusted: " + root.auth_previous_fingerprint; color: Theme.secondary; font-size: Theme.type-small; wrap: char-wrap; }
                                if root.auth_prompt_kind < 2: Rectangle {
                                    height: fingerprint-content.preferred-height; background: Theme.surface; border-radius: Theme.radius;
                                    fingerprint-content := VerticalLayout { padding: Theme.inset; spacing: Theme.gap;
                                        Text { text: root.auth_prompt_kind == 1 ? "Replacement fingerprint" : "Server fingerprint"; color: Theme.secondary; font-size: Theme.type-small; }
                                        Text { text: root.auth_fingerprint; color: Theme.ink; font-family: root.terminal_font_family; font-size: Theme.type-small; wrap: char-wrap; }
                                    }
                                }
                                if root.auth_prompt_kind < 2: HorizontalLayout {
                                    height: Theme.touch; spacing: Theme.inset;
                                    TouchButton { icon: "copy"; label: "Copy fingerprint"; activated => { root.copy-fingerprint(); } }
                                    TouchButton { icon: "shield"; label: "How to verify"; activated => { root.verification-help = !root.verification-help; } }
                                }
                                if root.auth_prompt_kind < 2 && root.verification-help: Text {
                                    text: "Ask the server owner for its fingerprint over a trusted channel. If you administer it, run ssh-keygen -lf on the server’s SSH host public-key file. Compare the complete SHA256 value and key type before trusting it.";
                                    color: Theme.secondary; font-size: Theme.type-small; wrap: word-wrap;
                                }
                                if root.auth_error != "": Text { text: root.auth_error; color: Theme.ink; font-size: Theme.type-small; wrap: word-wrap; }
                                prompt-secret := ConnectionField {
                                    visible: root.auth_prompt_kind >= 2;
                                    height: self.visible ? Theme.touch : 0px;
                                    label: root.auth_prompt_kind == 3 ? "Key passphrase" : "Password"; secret: true;
                                    value <=> root.auth_secret; enabled: root.local_editor_active && root.auth_prompt_visible;
                                    focused => {
                                        if self.y + prompt-scroll.viewport-y < 0px { prompt-scroll.viewport-y = -self.y; }
                                        else if self.y + self.height + prompt-scroll.viewport-y > prompt-scroll.height {
                                            prompt-scroll.viewport-y = min(0px, prompt-scroll.height - self.y - self.height);
                                        }
                                    }

                                }
                                if root.auth_prompt_kind == 2 && root.auth_password_saving_allowed: VerticalLayout {
                                    spacing: Theme.gap;
                                    HorizontalLayout { height: Theme.touch;
                                        SessionCheck { label: "Remember password on this Kindle"; checked: root.auth_remember_password; enabled: root.local_editor_active; toggled => { root.auth_remember_password = !root.auth_remember_password; } }
                                        Text { text: "Remember on this Kindle"; color: Theme.ink; vertical-alignment: center; }
                                    }
                                    if root.auth_remember_password: Text { text: "Stored as plaintext on this Kindle. Remove it in Settings, Credentials."; color: Theme.secondary; font-size: Theme.type-small; wrap: word-wrap; }
                                }
                                if root.auth_prompt_kind >= 2 && (root.auth_prompt_kind != 2 || !root.auth_password_saving_allowed): Text { text: "Used for this connection only. Never saved."; color: Theme.ink; wrap: word-wrap; }
                                HorizontalLayout {
                                    height: Theme.touch; spacing: Theme.inset;
                                    TouchButton {
                                        width: root.auth_prompt_kind < 2 ? min(Theme.touch * 4.5, parent.width - Theme.touch * 2 - Theme.inset * 2) : min(Theme.touch * 2.5, parent.width - Theme.touch * 4 - Theme.inset * 3);
                                        icon: root.auth_prompt_kind < 2 ? "shield" : "key";
                                        label: root.auth_prompt_kind == 0 ? "Trust host" : root.auth_prompt_kind == 1 ? "Replace trusted key" : root.auth_prompt_kind == 3 ? "Unlock" : "Sign in";
                                        active: true; enabled: root.application_active && !root.auth_busy;
                                        activated => { root.answer-auth(true); }
                                    }
                                    prompt-cancel := TouchButton { width: Theme.touch * 2; label: "Cancel"; enabled: root.application_active; activated => { root.clear-auth-secrets(); root.answer-auth(false); } }
                                    if root.auth_prompt_kind >= 2: TouchButton { width: Theme.touch * 2; label: "Paste"; enabled: root.local_editor_active; activated => { root.paste(); } }
                                    Rectangle { horizontal-stretch: 1; }
                                }
                            }
                        }
                    }
                }
          }
          Rectangle {
              x: 0px;
              y: Theme.chrome-height - Theme.rule;
              width: root.width;
              height: Theme.rule;
              background: Theme.ink;
          }
        }
    }
}
