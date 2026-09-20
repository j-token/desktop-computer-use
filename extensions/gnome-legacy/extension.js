const Gio = imports.gi.Gio;
const GLib = imports.gi.GLib;
const Clutter = imports.gi.Clutter;
const Meta = imports.gi.Meta;
const Shell = imports.gi.Shell;
const St = imports.gi.St;
const Main = imports.ui.main;
const PanelMenu = imports.ui.panelMenu;
const ExtensionUtils = imports.misc.extensionUtils;
const Me = ExtensionUtils.getCurrentExtension();

const BUS_NAME = 'org.desktopcomputeruse.Shell';
const OBJECT_PATH = '/org/desktopcomputeruse/Shell';
const INTERFACE_NAME = 'org.desktopcomputeruse.Shell';
const HEARTBEAT_TIMEOUT_US = 5 * 1000 * 1000;
const CURSOR_SIZE = 44;
const SCREEN_BORDER_WIDTH = 18;

const INTERFACE_XML = '<node>' +
    '<interface name="org.desktopcomputeruse.Shell">' +
    '<method name="Start"><arg name="sessionId" type="s" direction="in"/>' +
    '<arg name="result" type="s" direction="out"/></method>' +
    '<method name="Stop"><arg name="stopped" type="b" direction="out"/></method>' +
    '<method name="Heartbeat"><arg name="alive" type="b" direction="out"/></method>' +
    '<method name="ListWindows"><arg name="windows" type="s" direction="out"/></method>' +
    '<method name="Activate"><arg name="windowId" type="s" direction="in"/>' +
    '<arg name="activated" type="b" direction="out"/></method>' +
    '<method name="Pointer"><arg name="x" type="d" direction="in"/>' +
    '<arg name="y" type="d" direction="in"/>' +
    '<arg name="updated" type="b" direction="out"/></method>' +
    '<method name="GetPointer"><arg name="pointer" type="s" direction="out"/></method>' +
    '<method name="GetOverlayRegions"><arg name="regions" type="s" direction="out"/></method>' +
    '<signal name="Stopped"><arg name="reason" type="s"/></signal>' +
    '</interface></node>';

function jsonError(code, message) {
    return JSON.stringify({ready: false, error: {code: code, message: message}});
}

function finiteNumber(value) {
    var number = Number(value);
    return Number.isFinite(number) ? number : null;
}

var ShellService = class ShellService {
    constructor() {
        this._active = false;
        this._sessionId = '';
        this._lastHeartbeat = 0;
        this._heartbeatSource = 0;
        this._pointerSource = 0;
        this._monitorSignal = 0;
        this._ownerId = 0;
        this._connection = null;
        this._exported = null;
        this._settings = null;
        this._banner = null;
        this._panelIndicator = null;
        this._cursor = null;
        this._lastPointer = {x: 0, y: 0};
        this._screenBorders = [];
        this._stopKeyRegistered = false;
    }

    enable() {
        this._loadStylesheet();
        this._createUi();
        this._refreshScreenBorders();
        this._settings = ExtensionUtils.getSettings();
        this._monitorSignal = Main.layoutManager.connect('monitors-changed', () => {
            this._positionBanner();
            this._refreshScreenBorders();
        });
        this._ownerId = Gio.bus_own_name(
            Gio.BusType.SESSION,
            BUS_NAME,
            Gio.BusNameOwnerFlags.NONE,
            connection => this._onBusAcquired(connection),
            null,
            () => this._onBusLost());
        this._positionBanner();
    }

    disable() {
        this._stop('extension-disabled', false);
        if (this._heartbeatSource) {
            GLib.source_remove(this._heartbeatSource);
            this._heartbeatSource = 0;
        }
        if (this._pointerSource) {
            GLib.source_remove(this._pointerSource);
            this._pointerSource = 0;
        }
        if (this._monitorSignal) {
            Main.layoutManager.disconnect(this._monitorSignal);
            this._monitorSignal = 0;
        }
        if (this._ownerId) {
            Gio.bus_unown_name(this._ownerId);
            this._ownerId = 0;
        }
        this._unexport();
        this._destroyUi();
        this._unloadStylesheet();
    }

    _loadStylesheet() {
        try {
            var stylesheet = Me.dir.get_child('stylesheet.css');
            if (stylesheet.query_exists(null))
                St.ThemeContext.get_for_stage(global.stage).get_theme().load_stylesheet(stylesheet);
        } catch (error) {
            logError(error, 'Desktop Computer Use stylesheet failed to load');
        }
    }

    _unloadStylesheet() {
        try {
            var stylesheet = Me.dir.get_child('stylesheet.css');
            if (stylesheet.query_exists(null))
                St.ThemeContext.get_for_stage(global.stage).get_theme().unload_stylesheet(stylesheet);
        } catch (error) {
            logError(error, 'Desktop Computer Use stylesheet failed to unload');
        }
    }

    _createUi() {
        this._panelIndicator = new PanelMenu.Button(0.0, 'Desktop Computer Use', false);
        this._panelIndicator.reactive = false;
        this._panelIndicator.can_focus = false;
        this._panelIndicator.track_hover = false;
        this._panelIndicator.add_child(new St.Label({
            text: '● 컴퓨터 사용 중',
            style_class: 'dcu-panel-label',
            y_align: Clutter.ActorAlign.CENTER
        }));
        Main.panel.addToStatusArea('desktop-computer-use', this._panelIndicator, 0, 'center');
        this._panelIndicator.visible = false;

        this._banner = new St.BoxLayout({
            style_class: 'dcu-banner',
            reactive: false,
            can_focus: false
        });
        this._banner.add_child(new St.Label({
            text: '컴퓨터 사용 중  ·  중지: Esc',
            style_class: 'dcu-banner-label',
            y_align: Clutter.ActorAlign.CENTER
        }));
        Main.layoutManager.addChrome(this._banner, {
            affectsStruts: false,
            trackFullscreen: false
        });
        this._banner.visible = false;

        this._cursor = new St.Widget({
            style_class: 'dcu-cursor',
            reactive: false,
            can_focus: false
        });
        this._cursor.set_size(CURSOR_SIZE, CURSOR_SIZE);
        this._cursor.add_child(new St.Label({
            text: '➤',
            style_class: 'dcu-cursor-arrow'
        }));
        Main.layoutManager.addChrome(this._cursor, {
            affectsStruts: false,
            trackFullscreen: false
        });
        this._cursor.visible = false;
    }

    _registerStopKey() {
        if (this._stopKeyRegistered)
            return true;
        try {
            // Migrate any saved shortcut from earlier extension versions.
            if (!this._settings.set_strv('desktop-computer-use-stop-accelerator', ['Escape']))
                return false;
            const action = Main.wm.addKeybinding(
                'desktop-computer-use-stop-accelerator', this._settings,
                Meta.KeyBindingFlags.NONE, Shell.ActionMode.ALL,
                () => this._stop('escape'));
            if (action === Meta.KeyBindingAction.NONE)
                return false;
            this._stopKeyRegistered = true;
            return true;
        } catch (error) {
            logError(error, 'Desktop Computer Use could not register Esc');
            return false;
        }
    }

    _unregisterStopKey() {
        if (!this._stopKeyRegistered)
            return;
        this._stopKeyRegistered = false;
        Main.wm.removeKeybinding('desktop-computer-use-stop-accelerator');
    }

    _clearScreenBorders() {
        for (const border of this._screenBorders) {
            Main.layoutManager.removeChrome(border);
            border.destroy();
        }
        this._screenBorders = [];
    }

    _refreshScreenBorders() {
        this._clearScreenBorders();
        for (const monitor of Main.layoutManager.monitors) {
            const thickness = SCREEN_BORDER_WIDTH;
            const edges = [
                {side: 'top', x: monitor.x, y: monitor.y, width: monitor.width, height: thickness},
                {side: 'bottom', x: monitor.x, y: monitor.y + monitor.height - thickness, width: monitor.width, height: thickness},
                {side: 'left', x: monitor.x, y: monitor.y, width: thickness, height: monitor.height},
                {side: 'right', x: monitor.x + monitor.width - thickness, y: monitor.y, width: thickness, height: monitor.height},
            ];
            for (const edge of edges) {
                const border = new St.Widget({
                    style_class: 'dcu-screen-border-' + edge.side,
                    reactive: false,
                    can_focus: false,
                    visible: this._active,
                });
                border.set_position(edge.x, edge.y);
                border.set_size(edge.width, edge.height);
                Main.layoutManager.addChrome(border, {
                    affectsStruts: false,
                    affectsInputRegion: false,
                    trackFullscreen: false,
                });
                this._screenBorders.push(border);
            }
        }
    }

    _destroyUi() {
        this._clearScreenBorders();
        var actors = [this._cursor, this._banner];
        for (var i = 0; i < actors.length; i++) {
            var actor = actors[i];
            if (!actor)
                continue;
            try {
                Main.layoutManager.removeChrome(actor);
            } catch (error) {
                logError(error, 'Desktop Computer Use could not remove shell chrome');
            }
            actor.destroy();
        }
        if (this._panelIndicator)
            this._panelIndicator.destroy();
        this._cursor = null;
        this._banner = null;
        this._panelIndicator = null;
    }

    _positionBanner() {
        if (!this._banner)
            return;
        var monitor = Main.layoutManager.primaryMonitor;
        if (!monitor)
            return;
        var preferred = this._banner.get_preferred_width(-1);
        var naturalWidth = preferred[1];
        this._banner.set_position(
            Math.round(monitor.x + (monitor.width - naturalWidth) / 2),
            monitor.y + 8);
    }

    _showUi() {
        if (!this._registerStopKey())
            return false;
        for (const border of this._screenBorders)
            border.visible = true;
        if (!this._banner || !this._cursor || !this._panelIndicator)
            return false;
        this._positionBanner();
        this._banner.visible = true;
        this._cursor.visible = true;
        this._panelIndicator.visible = true;
        this._updatePointerFromShell();
        if (!this._pointerSource) {
            this._pointerSource = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 16, () => {
                if (!this._active || !this._cursor)
                    return GLib.SOURCE_REMOVE;
                this._updatePointerFromShell();
                return GLib.SOURCE_CONTINUE;
            });
        }
        return this._banner.visible && this._cursor.visible && this._panelIndicator.visible;
    }

    _hideUi() {
        this._unregisterStopKey();
        for (const border of this._screenBorders)
            border.visible = false;
        if (this._banner)
            this._banner.visible = false;
        if (this._cursor)
            this._cursor.visible = false;
        if (this._panelIndicator)
            this._panelIndicator.visible = false;
        if (this._pointerSource) {
            GLib.source_remove(this._pointerSource);
            this._pointerSource = 0;
        }
    }

    _updatePointerFromShell() {
        try {
            var pointer = global.get_pointer();
            this._moveCursor(finiteNumber(pointer[0]), finiteNumber(pointer[1]));
        } catch (error) {
            logError(error, 'Desktop Computer Use could not read shell pointer');
        }
    }

    _moveCursor(x, y) {
        if (x === null || y === null || !this._cursor)
            return false;
        this._lastPointer = {x: x, y: y};
        this._cursor.set_position(Math.round(x - CURSOR_SIZE / 2),
            Math.round(y - CURSOR_SIZE / 2));
        return true;
    }

    _ensureHeartbeatWatch() {
        if (this._heartbeatSource)
            return;
        this._heartbeatSource = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, 1, () => {
            if (!this._active)
                return GLib.SOURCE_REMOVE;
            if (GLib.get_monotonic_time() - this._lastHeartbeat > HEARTBEAT_TIMEOUT_US)
                this._stop('heartbeat-timeout');
            return this._active ? GLib.SOURCE_CONTINUE : GLib.SOURCE_REMOVE;
        });
    }

    _onBusAcquired(connection) {
        this._connection = connection;
        var implementation = {
            Start: sessionId => this.Start(sessionId),
            Stop: () => this.Stop(),
            Heartbeat: () => this.Heartbeat(),
            ListWindows: () => this.ListWindows(),
            Activate: windowId => this.Activate(windowId),
            Pointer: (x, y) => this.Pointer(x, y),
            GetPointer: () => this.GetPointer(),
            GetOverlayRegions: () => this.GetOverlayRegions()
        };
        this._exported = Gio.DBusExportedObject.wrapJSObject(INTERFACE_XML, implementation);
        try {
            this._exported.export(connection, OBJECT_PATH);
        } catch (error) {
            logError(error, 'Desktop Computer Use D-Bus export failed');
            this._exported = null;
        }
    }

    _onBusLost() {
        this._unexport();
        if (this._active)
            this._stop('bus-lost', false);
    }

    _unexport() {
        if (!this._exported)
            return;
        try {
            this._exported.unexport();
        } catch (error) {
            logError(error, 'Desktop Computer Use D-Bus unexport failed');
        }
        this._exported = null;
        this._connection = null;
    }

    _stop(reason, emitSignal) {
        if (emitSignal === undefined)
            emitSignal = true;
        var wasActive = this._active;
        this._active = false;
        this._sessionId = '';
        this._lastHeartbeat = 0;
        if (this._heartbeatSource) {
            GLib.source_remove(this._heartbeatSource);
            this._heartbeatSource = 0;
        }
        this._hideUi();
        if (wasActive && emitSignal && this._exported) {
            try {
                this._exported.emit_signal('Stopped', new GLib.Variant('(s)', [String(reason)]));
            } catch (error) {
                logError(error, 'Desktop Computer Use could not emit Stopped');
            }
        }
        return wasActive;
    }

    Start(sessionId) {
        if (typeof sessionId !== 'string' || sessionId.trim() === '')
            return jsonError('invalid_session', 'sessionId must be a non-empty string');
        if (this._active && this._sessionId !== sessionId)
            return jsonError('busy', 'another computer use session is active');
        this._sessionId = sessionId;
        this._active = true;
        this._lastHeartbeat = GLib.get_monotonic_time();
        var ready = this._showUi();
        this._ensureHeartbeatWatch();
        if (!ready) {
            this._stop('indicator-not-ready');
            return jsonError('indicator_not_ready', 'shell indicator could not be shown');
        }
        return JSON.stringify({
            ready: true,
            sessionId: sessionId,
            bus: BUS_NAME,
            objectPath: OBJECT_PATH,
            interface: INTERFACE_NAME,
            stopShortcut: 'Esc',
            cursor: 'high-contrast-ring-arrow'
        });
    }

    Stop() {
        return this._stop('requested');
    }

    Heartbeat() {
        if (!this._active)
            return false;
        this._lastHeartbeat = GLib.get_monotonic_time();
        return true;
    }

    ListWindows() {
        try {
            return JSON.stringify(this._getWindows());
        } catch (error) {
            logError(error, 'Desktop Computer Use could not enumerate windows');
            return '[]';
        }
    }

    Activate(windowId) {
        if (!this._active || typeof windowId !== 'string')
            return false;
        var actors = global.get_window_actors();
        for (var i = 0; i < actors.length; i++) {
            var window = actors[i].meta_window;
            if (!window || this._windowId(window) !== windowId)
                continue;
            try {
                if (window.minimized)
                    window.unminimize(global.get_current_time());
                // Shell activation also leaves overview and selects the target workspace.
                Main.activateWindow(window);
                return true;
            } catch (error) {
                logError(error, 'Desktop Computer Use could not activate window');
                return false;
            }
        }
        return false;
    }

    Pointer(x, y) {
        if (!this._active)
            return false;
        return this._moveCursor(finiteNumber(x), finiteNumber(y));
    }

    GetOverlayRegions() {
        if (!this._active)
            return '[]';
        const actors = this._screenBorders.map(actor => ({actor, kind: 'screen-border'}));
        actors.push({actor: this._banner, kind: 'banner'});
        actors.push({actor: this._cursor, kind: 'cursor'});
        actors.push({actor: this._panelIndicator, kind: 'panel'});
        const regions = [];
        for (const entry of actors) {
            if (!entry.actor || !entry.actor.visible)
                continue;
            const [x, y] = entry.actor.get_transformed_position();
            const [width, height] = entry.actor.get_transformed_size();
            regions.push({kind: entry.kind, x, y, width, height});
        }
        return JSON.stringify(regions);
    }

    GetPointer() {
        try {
            var pointer = global.get_pointer();
            return JSON.stringify({x: Number(pointer[0]), y: Number(pointer[1])});
        } catch (error) {
            logError(error, 'Desktop Computer Use could not read pointer');
            return JSON.stringify(this._lastPointer);
        }
    }

    _windowId(window) {
        var nativeId;
        try {
            if (typeof window.get_stable_sequence === 'function')
                nativeId = 'stable-' + String(window.get_stable_sequence());
            else
                nativeId = window.get_id();
        } catch (error) {
            nativeId = String(window.get_pid ? window.get_pid() : 0) + ':' +
                String(window.get_title ? window.get_title() : '');
        }
        return 'window-' + String(nativeId);
    }

    _windowApp(window) {
        try {
            var app = Shell.WindowTracker.get_default().get_window_app(window);
            if (app)
                return app.get_name() || app.get_id() || '';
        } catch (error) {
            // A windowless game can have no Shell.App; use WM metadata below.
        }
        try {
            return (window.get_wm_class ? window.get_wm_class() : '') ||
                (window.get_wm_class_instance ? window.get_wm_class_instance() : '') || '';
        } catch (error) {
            return '';
        }
    }

    _getWindows() {
        var windows = [];
        var seen = new Set();
        var actors = global.get_window_actors();
        for (var i = 0; i < actors.length; i++) {
            var window = actors[i].meta_window;
            if (!window)
                continue;
            var type;
            try {
                type = window.get_window_type();
            } catch (error) {
                continue;
            }
            if (type === Meta.WindowType.DESKTOP || type === Meta.WindowType.DOCK)
                continue;
            var id = this._windowId(window);
            if (seen.has(id))
                continue;
            seen.add(id);
            var rect;
            try {
                rect = window.get_frame_rect();
            } catch (error) {
                continue;
            }
            var title = '';
            try {
                title = window.get_title() || '';
            } catch (error) {
                // Keep empty titles: they are useful for borderless/windowless games.
            }
            windows.push({
                id: id,
                title: title,
                app: this._windowApp(window),
                pid: Number(window.get_pid ? window.get_pid() : 0),
                x: Number(rect.x),
                y: Number(rect.y),
                width: Number(rect.width),
                height: Number(rect.height)
            });
        }
        return windows;
    }
};

var DesktopComputerUseExtension = class DesktopComputerUseExtension {
    enable() {
        this._service = new ShellService();
        this._service.enable();
    }

    disable() {
        if (this._service) {
            this._service.disable();
            this._service = null;
        }
    }
};

function init() {
    return new DesktopComputerUseExtension();
}
