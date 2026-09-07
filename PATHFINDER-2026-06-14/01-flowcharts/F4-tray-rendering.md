# F4 — System Tray Rendering

**Feature Scope:** Tauri system tray icon with stacked progress bars, context menu, and 250ms refresh debounce.

**Entry Points:**
- `src-tauri/src/bootstrap.rs:43` (`manage_app_state_and_tray`) wires tray setup into app initialization
- `src-tauri/src/tray/service.rs:198` (`setup_tray`) creates the initial tray icon
- Refresh triggered by watcher events via `record_tray_refresh_request` → `request_tray_refresh` debounce

---

## Happy Path

**Startup (synchronous, blocking):**
1. `manage_app_state_and_tray` (`bootstrap.rs:43`) calls `setup_tray`
2. `setup_tray` (`service.rs:198`) generates initial icon using `startup_tray_update` → empty 3-bar PNG
3. Icon, tooltip, and menu built; `TrayIconBuilder` with handlers attached
4. Icon installed into system tray; `request_tray_refresh` spawns first async refresh task (debounced 250ms)

**Per-Refresh Loop (async, triggered by watcher or startup):**
1. `request_tray_refresh` (`service.rs:173`) atomically sets `TRAY_REFRESH_PENDING` flag
2. Spawns Tokio task → sleeps 250ms (`TRAY_REFRESH_DEBOUNCE_MS`)
3. Clears flag; calls `refresh_tray` (`service.rs:230`)
4. `refresh_tray` loads current app state and rebuilds tray state
5. Updates icon, tooltip, and menu in system tray; emits to UI

**Data Flow (Model Build):**
1. `build_tray_state_for_app` (`service.rs:78`) → loads projects from DB
2. `project_repo::list_project_snapshots` returns `Vec<StoredProjectSnapshot>`
3. `build_tray_state_from_parts` (`service.rs:96`) converts snapshots:
   - Map `StoredProjectSnapshot` → `TrayProject` (includes `next_command`)
   - Filter by visibility settings via `visible_tray_projects` (`model.rs:69`)
   - Sort by user preference (Name, Progress, RecentActivity)
   - Truncate to `adaptive_bar_count` (fit width constraint)
   - Build `HashMap<project_id, next_command>`
4. `tray_summary` (`service.rs:138`) computes portfolio stats (count, avg progress)

**Render Path (PNG Generation):**
1. `render_tray_icon_png` (`render.rs:9`) receives visible projects + `TrayRenderSpec`
2. Spec includes: width (computed from count), height (44px on macOS templates), scale, macOS flag
3. Create `tiny_skia::Pixmap` (width × height)
4. `draw_bars` (`render.rs:52`) iterates projects:
   - Calculate bar height from `milestone_progress_pct` (clamped 0–100%)
   - Enforce `MIN_VISIBLE_HEIGHT_PX` (2px minimum if progress > 0)
   - Draw black RGBA rectangle at computed (x, y) with gap between bars
5. `pixmap.encode_png()` returns `Vec<u8>` PNG bytes

**Menu Build (Context Menu):**
1. `build_native_menu` (`service.rs:307`) creates root menu with:
   - "Show Dashboard" (navigation to `/`)
   - "Preferences" (navigation to `/settings`)
   - Portfolio overview (disabled, shows project count + avg progress)
   - Separator
2. For each visible project, append submenu:
   - Project name + progress % (label)
   - Detail line (read-only, shows progress graph + activity status)
   - "Copy Next Command" action (copies stored `next_command` to clipboard)
3. Separator + "Quit" at bottom

**Event Handlers (Click & Menu Selection):**

*Left-click tray icon:*
- `on_tray_icon_event` (`service.rs:209`) detects `MouseButton::Left` + `MouseButtonState::Up`
- Calls `toggle_dashboard_window` (`service.rs:369`)
- If window visible + focused → hide; else show/focus
- If window doesn't exist → create via `show_dashboard_window`

*Menu selection:*
- `on_menu_event` (`service.rs:206`) triggers `dispatch_menu_action` (`service.rs:259`)
- Spawns async task to resolve action ID → `current_menu_action` → `resolve_menu_action`
- **ShowDashboard / Preferences / OpenProject:** call `show_dashboard_window` with route; emit `AppEvent::TrayNavigate` to UI
- **CopyNextCommand:** rebuild tray state, look up command in `commands_by_project_id`, write to clipboard via `app.clipboard().write_text()`
- **Quit:** `app.exit(0)`

**macOS Template Branch:**
- `#[cfg(target_os = "macos")]` → `icon_as_template(true)` on builder (`service.rs:220`)
- `apply_macos_template_icon` (`service.rs:249`) calls `tray.set_icon_as_template(true)` post-render
- Render spec enforces 44px height; returns error if violated

---

## Side Effects

| Effect | Trigger | Function | Line |
|--------|---------|----------|------|
| **PNG encode (tiny-skia)** | Each render | `pixmap.encode_png()` | `render.rs:38` |
| **Icon set (Tauri tray)** | Setup + refresh | `tray.set_icon(Some(icon))` | `service.rs:240` |
| **Tooltip set** | Setup + refresh | `tray.set_tooltip(Some(&update.tooltip))` | `service.rs:242` |
| **Menu set** | Setup + refresh | `tray.set_menu(Some(menu))` | `service.rs:245` |
| **macOS template flag** | Setup + refresh | `tray.set_icon_as_template(true)` | `service.rs:251` |
| **Clipboard write** | "Copy Next Command" action | `app.clipboard().write_text(command)` | `service.rs:292` |
| **Window show/hide/focus** | Left-click or "Show Dashboard" | `window.show()`, `window.hide()`, `window.set_focus()` | `service.rs:372–376` |
| **Event emit** | Menu navigation action | `app.emit_to(MAIN_WINDOW_LABEL, "trayNavigate", ...)` | `service.rs:274–277` |
| **Tokio task spawn** | `request_tray_refresh` | `tauri::async_runtime::spawn(async move { ... })` | `service.rs:182` |
| **Debounce timer** | Refresh spawn | `tokio::time::sleep(Duration::from_millis(250))` | `service.rs:183` |
| **AtomicBool CAS** | Debounce guard | `TRAY_REFRESH_PENDING.compare_exchange()` | `service.rs:174–176` |

---

## Flowchart

```mermaid
flowchart TD
    A["App Boot<br/>bootstrap:43"] -->|manage_app_state_and_tray| B["Setup Tray<br/>service:198"]
    B -->|startup_tray_update| C["Render Empty Icon<br/>render:9"]
    C -->|render_tray_icon_png| D["Create Pixmap<br/>render:23"]
    D -->|empty_baseline_bars| E["Draw 3 Bars<br/>render:52"]
    E -->|pixmap.encode_png| F["PNG Vec&lt;u8&gt;<br/>render:38"]
    
    F -->|Image::from_bytes| G["Icon Object<br/>service:200"]
    G -->|build_native_menu| H["Build Root Menu<br/>service:307"]
    H -->|for each project| I["Append Project Submenu<br/>service:354-360"]
    
    G -->|TrayIconBuilder| J["Register Event Handlers<br/>service:202-218"]
    J -->|on_menu_event| K["dispatch_menu_action<br/>service:259"]
    J -->|on_tray_icon_event| L["Left-Click Handler<br/>service:209-217"]
    
    J -->|build tray| M["TrayIcon Installed<br/>service:222"]
    M -->|apply_macos_template| N["Set macOS Template Flag<br/>service:249-251"]
    N -->|#[cfg target_os]| O["icon_as_template true<br/>service:220,251"]
    O -->|OR skip non-macOS| P["No-op on Linux/Windows<br/>service:253-254"]
    
    M -->|request_tray_refresh| Q["Spawn Debounce Task<br/>service:173"]
    Q -->|AtomicBool CAS| R["Set TRAY_REFRESH_PENDING<br/>service:174-176"]
    R -->|tauri::async_runtime::spawn| S["Async Task<br/>service:182"]
    S -->|sleep| T["Wait 250ms<br/>service:183"]
    T -->|store false| U["Clear Pending Flag<br/>service:184"]
    U -->|refresh_tray| V["Load App State<br/>service:234"]
    
    V -->|build_tray_state_for_app| W["Load DB Connection<br/>service:78-84"]
    W -->|project_repo::list_project_snapshots| X["Get All Snapshots<br/>service:81-84"]
    X -->|map TrayProject::from| Y["Convert to TrayProject<br/>service:103-106"]
    Y -->|visible_tray_projects| Z["Filter Hide-Hidden<br/>model:69-96"]
    Z -->|sort_tray_projects| AA["Sort by Preference<br/>model:107-130"]
    AA -->|tray_render_spec_for_projects| AB["Compute Render Spec<br/>model:50-65"]
    AB -->|adaptive_bar_count| AC["Fit Width Constraint<br/>model:99-105"]
    AC -->|truncate| AD["Final Project List<br/>service:115"]
    
    AD -->|build commands map| AE["Map ID → next_command<br/>service:120-124"]
    AD -->|tray_summary| AF["Portfolio Summary<br/>service:138-152"]
    AF -->|render_tray_icon_png| AG["Render Progress Icon<br/>render:9"]
    AG -->|draw_bars| AH["Calculate Bar Heights<br/>render:71-80"]
    AH -->|pixmap.fill_rect| AI["Draw Black Bars<br/>render:81-84"]
    AI -->|pixmap.encode_png| AJ["Encode PNG<br/>render:38"]
    
    AJ -->|native_tray_update| AK["Build Update Payload<br/>service:56-61"]
    AK -->|tray_by_id| AL["Get Tray Instance<br/>service:236-238"]
    AL -->|Image::from_bytes| AM["Icon from PNG<br/>service:239"]
    AM -->|set_icon| AN["Update Icon<br/>service:240"]
    AN -->|set_tooltip| AO["Update Tooltip<br/>service:242"]
    AO -->|set_menu| AP["Rebuild Native Menu<br/>service:244-245"]
    
    K -->|current_menu_action| AQ["Resolve Action<br/>service:301-305"]
    AQ -->|build_tray_state_for_app| AR["Get Fresh State<br/>service:303"]
    AR -->|resolve_menu_action| AS["Validate & Return Action<br/>service:156-169"]
    
    AS -->|ShowDashboard| AT["Get Navigation Route<br/>menu.rs:18-23"]
    AT -->|show_dashboard_window| AU["Show Window<br/>service:386-394"]
    AU -->|emit_to| AV["Emit TrayNavigate Event<br/>service:274-277"]
    
    AS -->|OpenProject| AW["Get Project Route<br/>menu.rs:20"]
    AW -->|show_dashboard_window| AX["Show at Route<br/>service:386"]
    AX -->|emit_to| AY["Emit TrayNavigate Event<br/>service:274-277"]
    
    AS -->|CopyNextCommand| AZ["Lookup Command ID<br/>service:291"]
    AZ -->|app.clipboard| BA["Write to Clipboard<br/>service:292"]
    
    AS -->|Quit| BB["app.exit 0<br/>service:296"]
    
    AS -->|Preferences| BC["Get Settings Route<br/>menu.rs:19"]
    BC -->|show_dashboard_window| BD["Show at /settings<br/>service:386"]
    BD -->|emit_to| BE["Emit TrayNavigate Event<br/>service:274-277"]
    
    L -->|toggle_dashboard_window| BF["Check Window State<br/>service:369-376"]
    BF -->|is_visible & is_focused| BG{"Window Visible<br/>& Focused?"}
    BG -->|Yes| BH["Hide Window<br/>service:372"]
    BG -->|No| BI["Show & Focus<br/>service:374-376"]
    
    BF -->|window not found| BJ["show_dashboard_window<br/>service:379"]
    BJ -->|create new window| BK["WebviewWindowBuilder<br/>service:400"]
    
    style A fill:#4a90e2
    style B fill:#4a90e2
    style F fill:#f5a623
    style G fill:#f5a623
    style H fill:#7ed321
    style M fill:#bd10e0
    style N fill:#bd10e0
    style O fill:#f8e71c
    style Q fill:#50e3c2
    style S fill:#50e3c2
    style T fill:#50e3c2
    style U fill:#50e3c2
    style V fill:#9013fe
    style W fill:#9013fe
    style X fill:#9013fe
    style AJ fill:#f5a623
    style AK fill:#f5a623
    style AN fill:#bd10e0
    style AO fill:#bd10e0
    style AP fill:#7ed321
    style AQ fill:#ff6b6b
    style AS fill:#ff6b6b
    style AU fill:#4ecdc4
    style AV fill:#95e1d3
    style BA fill:#d4af37
    style BB fill:#ff0000
    style BF fill:#4ecdc4
    style BG fill:#ffd700
```

---

## External Dependencies

| Crate | Module | Usage |
|-------|--------|-------|
| **tiny-skia** | `Pixmap`, `Paint`, `encode_png` | PNG rasterization of progress bars |
| **tauri** | `TrayIcon`, `TrayIconBuilder`, `TrayIconEvent`, `Image` | Tray icon installation, event handling |
| **tauri** | `Menu`, `MenuItem`, `Submenu`, `PredefinedMenuItem` | Context menu construction |
| **tauri** | `AppHandle`, `Manager`, `Runtime`, `Emitter` | App state & event dispatch |
| **tauri-plugin-clipboard-manager** | `ClipboardExt` | Clipboard write for "Copy Next Command" |
| **tokio** | `time::sleep`, `task::spawn` (via `tauri::async_runtime`) | Debounce timer and async task spawn |
| **std::sync::atomic** | `AtomicBool`, `Ordering` | Debounce guard (CAS operation) |

---

## Sources Consulted

- `/Users/smacdonald/homegit/gsd-dashboard/src-tauri/src/tray/service.rs` (lines 1–415)
- `/Users/smacdonald/homegit/gsd-dashboard/src-tauri/src/tray/render.rs` (lines 1–95)
- `/Users/smacdonald/homegit/gsd-dashboard/src-tauri/src/tray/model.rs` (lines 1–140)
- `/Users/smacdonald/homegit/gsd-dashboard/src-tauri/src/tray/menu.rs` (full)
- `/Users/smacdonald/homegit/gsd-dashboard/src-tauri/src/bootstrap.rs` (lines 43–54)

---

## Confidence & Gaps

**Confidence: HIGH**
- Entire happy path traced: startup → model build → render → menu → event loop
- All side effects identified and located (PNG encode, icon set, clipboard, menu rebuild, Tokio spawn, debounce timer)
- macOS template branch and Linux left-click handling verified in source
- Line numbers pinpointed in all critical functions

**Gaps (Intentional Out-of-Scope):**
- Watcher → `record_tray_refresh_request` pathway (belongs in F3)
- Database connection pool & project snapshot schema (belongs in storage/data layer)
- Frontend route handling post-emit (belongs in React layer)
- Window creation/management full lifecycle (partial; F4 only covers tray-triggered show)
- Settings load/apply (loaded in `build_tray_state_for_app` but not traced into settings module)
