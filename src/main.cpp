#define WLR_USE_UNSTABLE

#include <hyprland/src/Compositor.hpp>
#include <hyprland/src/config/shared/actions/ConfigActions.hpp>
#include <hyprland/src/debug/log/Logger.hpp>
#include <hyprland/src/desktop/state/FocusState.hpp>
#include <hyprland/src/devices/IKeyboard.hpp>
#include <hyprland/src/event/EventBus.hpp>
#include <hyprland/src/output/Monitor.hpp>
#include <hyprland/src/plugins/PluginAPI.hpp>
#include <hyprland/src/state/MonitorState.hpp>
#include <hyprland/src/state/workspace/State.hpp>
#include <hyprland/src/workspace/HLWorkspace.hpp>

#include <linux/input-event-codes.h>
#include <lua.hpp>

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <ctime>
#include <fstream>
#include <iomanip>
#include <map>
#include <optional>
#include <sstream>
#include <string>
#include <vector>

inline HANDLE PHANDLE = nullptr;

namespace {

constexpr int MAX_WORKSPACE = 9;

struct SCycleState {
    std::string      monitorKey;
    int              originalWorkspace = 0;
    int              previewWorkspace  = 0;
    std::vector<int> history;
    size_t           nextIndex = 0;
};

class CWorkspaceHistory {
  public:
    void seedActiveWorkspaces() {
        if (!g_pCompositor)
            return;

        for (const auto& monitor : State::monitorState()->monitors()) {
            if (monitor && monitor->m_activeWorkspace)
                remember(monitor->m_activeWorkspace);
        }

        writeDebug("seed");
    }

    void observe(PHLWORKSPACE workspace) {
        const auto workspaceId = workspaceID(workspace);
        if (!workspaceId) {
            writeDebug("observe-skipped-non-normal-workspace");
            return;
        }

        const auto key = monitorKey(workspace);
        if (!m_cycle || m_cycle->monitorKey != key) {
            remember(workspace);
            return;
        }

        if (contains(m_cycle->history, *workspaceId)) {
            m_cycle->previewWorkspace = *workspaceId;
            writeDebug("cycle-observe-preview");
            return;
        }

        m_cycle.reset();
        remember(workspace);
        writeDebug("remember-after-cycle-abandoned");
    }

    SDispatchResult cycle(int direction) {
        auto* cycle = startCycle();
        if (!cycle)
            return {};

        if (cycle->history.size() < 2) {
            writeDebug("cycle-skipped-short-history");
            return {};
        }

        const auto target = cycle->history[cycle->nextIndex];
        cycle->previewWorkspace = target;
        const auto result = focusWorkspace(target);
        if (!result.success) {
            writeDebug("cycle-focus-failed");
            return result;
        }

        cycle->nextIndex = wrappedIndex(cycle->nextIndex, direction, cycle->history.size());
        writeDebug(std::string("cycle-preview-") + std::to_string(target));
        return {};
    }

    SDispatchResult commit() {
        if (!m_cycle) {
            writeDebug("commit-skipped-no-cycle");
            return {};
        }

        auto cycle = *m_cycle;
        m_cycle.reset();
        m_histories[cycle.monitorKey] = promote(cycle.history, cycle.previewWorkspace);
        writeDebug("commit");
        return {};
    }

    SDispatchResult cancel() {
        if (!m_cycle) {
            writeDebug("cancel-skipped-no-cycle");
            return {};
        }

        const auto original = m_cycle->originalWorkspace;
        m_cycle.reset();
        focusWorkspace(original);
        writeDebug("cancel");
        return {};
    }

    void onKey(IKeyboard::SKeyEvent event) {
        if (!m_cycle || event.state != WL_KEYBOARD_KEY_STATE_RELEASED)
            return;

        if (event.keycode == KEY_LEFTMETA || event.keycode == KEY_RIGHTMETA)
            commit();
    }

    std::string snapshot(const std::string& reason) const {
        return snapshotText(reason, m_revision);
    }

    std::string snapshotText(const std::string& reason, uint64_t revision) const {
        std::stringstream out;
        out << "reason=" << reason << "\n";
        out << "revision=" << revision << "\n";

        const auto monitor = Desktop::focusState() ? Desktop::focusState()->monitor() : nullptr;
        const auto workspace = monitor ? monitor->m_activeWorkspace : nullptr;
        out << "active_monitor=" << monitorKey(monitor) << "\n";
        const auto workspaceId = workspace ? workspace->numberedID() : std::nullopt;
        out << "active_workspace=" << (workspaceId ? std::to_string(*workspaceId) : "?") << "\n";

        for (const auto& [key, history] : m_histories) {
            out << "history." << key << "=";
            for (size_t i = 0; i < history.size(); ++i) {
                if (i > 0)
                    out << ",";
                out << history[i];
            }
            out << "\n";
        }

        if (m_cycle) {
            out << "cycle.monitor=" << m_cycle->monitorKey << "\n";
            out << "cycle.original=" << m_cycle->originalWorkspace << "\n";
            out << "cycle.preview=" << m_cycle->previewWorkspace << "\n";
            out << "cycle.next_index=" << (m_cycle->nextIndex + 1) << "\n";
            out << "cycle.history=";
            for (size_t i = 0; i < m_cycle->history.size(); ++i) {
                if (i > 0)
                    out << ",";
                out << m_cycle->history[i];
            }
            out << "\n";
        } else {
            out << "cycle=none\n";
        }

        return out.str();
    }

    std::string snapshotJson(const std::string& reason, uint64_t revision) const {
        std::stringstream out;

        const auto monitor = Desktop::focusState() ? Desktop::focusState()->monitor() : nullptr;
        const auto workspace = monitor ? monitor->m_activeWorkspace : nullptr;

        out << "{";
        out << "\"version\":1";
        out << ",\"revision\":" << revision;
        out << ",\"reason\":" << jsonString(reason);
        out << ",\"active_monitor\":" << jsonString(monitorKey(monitor));
        out << ",\"active_workspace\":";
        if (const auto workspaceId = workspace ? workspace->numberedID() : std::nullopt)
            out << *workspaceId;
        else
            out << "null";

        out << ",\"monitors\":{";
        bool firstMonitor = true;
        for (const auto& [key, history] : m_histories) {
            if (!firstMonitor)
                out << ",";
            firstMonitor = false;
            out << jsonString(key) << ":{\"history\":";
            writeJsonIntArray(out, history);
            out << "}";
        }
        out << "}";

        out << ",\"cycle\":";
        if (m_cycle) {
            out << "{";
            out << "\"monitor\":" << jsonString(m_cycle->monitorKey);
            out << ",\"original\":" << m_cycle->originalWorkspace;
            out << ",\"preview\":" << m_cycle->previewWorkspace;
            out << ",\"next_index\":" << (m_cycle->nextIndex + 1);
            out << ",\"history\":";
            writeJsonIntArray(out, m_cycle->history);
            out << "}";
        } else {
            out << "null";
        }

        out << "}\n";
        return out.str();
    }

    void showDebug() const {
        HyprlandAPI::addNotification(PHANDLE, snapshot("notification"), CHyprColor{0.4, 0.8, 1.0, 1.0}, 6000);
    }

  private:
    std::map<std::string, std::vector<int>> m_histories;
    std::optional<SCycleState>              m_cycle;
    mutable uint64_t                        m_revision = 0;

    static std::optional<int> workspaceID(PHLWORKSPACE workspace) {
        const auto workspaceId = workspace ? workspace->numberedID() : std::nullopt;
        if (!workspaceId || *workspaceId < 1 || *workspaceId > MAX_WORKSPACE)
            return std::nullopt;

        return static_cast<int>(*workspaceId);
    }

    static std::string monitorKey(PHLMONITOR monitor) {
        if (!monitor)
            return "unknown";

        if (!monitor->m_name.empty())
            return monitor->m_name;

        return std::to_string(monitor->m_id);
    }

    static std::string monitorKey(PHLWORKSPACE workspace) {
        if (!workspace || !workspace->m_monitor)
            return monitorKey(Desktop::focusState() ? Desktop::focusState()->monitor() : nullptr);

        return monitorKey(workspace->m_monitor.lock());
    }

    static bool contains(const std::vector<int>& history, int workspace) {
        return std::ranges::contains(history, workspace);
    }

    static std::vector<int> promote(std::vector<int> history, int workspace) {
        std::erase(history, workspace);
        history.insert(history.begin(), workspace);
        return history;
    }

    static size_t wrappedIndex(size_t current, int direction, size_t size) {
        const auto signedSize = static_cast<int>(size);
        auto       next       = static_cast<int>(current) + direction;
        next                  = ((next % signedSize) + signedSize) % signedSize;
        return static_cast<size_t>(next);
    }

    PHLWORKSPACE activeWorkspace() const {
        const auto monitor = Desktop::focusState() ? Desktop::focusState()->monitor() : nullptr;
        return monitor ? monitor->m_activeWorkspace : nullptr;
    }

    void remember(PHLWORKSPACE workspace) {
        const auto workspaceId = workspaceID(workspace);
        if (!workspaceId) {
            writeDebug("remember-skipped-non-normal-workspace");
            return;
        }

        const auto key = monitorKey(workspace);
        auto&      history = m_histories[key];
        const bool changed = history.empty() || history.front() != *workspaceId;
        history            = promote(history, *workspaceId);

        if (changed)
            writeDebug("remember");
    }

    SCycleState* startCycle() {
        if (m_cycle)
            return &*m_cycle;

        const auto workspace = activeWorkspace();
        remember(workspace);

        const auto workspaceId = workspaceID(workspace);
        if (!workspaceId) {
            writeDebug("cycle-start-skipped-non-normal-workspace");
            return nullptr;
        }

        const auto key = monitorKey(workspace);
        auto       history = promote(m_histories[key], *workspaceId);

        if (history.size() < 2) {
            writeDebug("cycle-start-skipped-short-history");
            return nullptr;
        }

        m_cycle = SCycleState{
            .monitorKey         = key,
            .originalWorkspace  = *workspaceId,
            .previewWorkspace   = *workspaceId,
            .history            = history,
            .nextIndex          = 1,
        };
        writeDebug("cycle-start");
        return &*m_cycle;
    }

    static SDispatchResult focusWorkspace(int workspace) {
        const auto monitor = Desktop::focusState() ? Desktop::focusState()->monitor() : nullptr;
        if (!monitor)
            return {.success = false, .error = "no focused monitor"};

        const auto id     = Workspace::SWorkspaceNumberedID{static_cast<Workspace::WorkspaceIDContainer>(workspace)};
        auto       target = State::Workspace::state()->query().numbered(id).run();
        if (!target)
            target = State::Workspace::state()->createNumbered(id, monitor, "", false);
        if (!target)
            return {.success = false, .error = "workspace " + std::to_string(workspace) + " is unavailable"};

        // The string overload would apply binds:workspace_back_and_forth.
        const auto result = Config::Actions::changeWorkspaceOnCurrentMonitor(target);
        target->ready();
        if (!result)
            return {.success = false, .error = result.error().message};

        return {.passEvent = result->passEvent};
    }

    static std::optional<std::string> runtimePath(const std::string& name) {
        const auto runtimeDir = std::getenv("XDG_RUNTIME_DIR");
        if (!runtimeDir)
            return std::nullopt;

        return std::string(runtimeDir) + "/" + name;
    }

    static std::string jsonString(const std::string& value) {
        std::stringstream out;
        out << '"';
        for (const char ch : value) {
            switch (ch) {
                case '"': out << "\\\""; break;
                case '\\': out << "\\\\"; break;
                case '\b': out << "\\b"; break;
                case '\f': out << "\\f"; break;
                case '\n': out << "\\n"; break;
                case '\r': out << "\\r"; break;
                case '\t': out << "\\t"; break;
                default:
                    if (static_cast<unsigned char>(ch) < 0x20) {
                        out << "\\u" << std::hex << std::setw(4) << std::setfill('0') << static_cast<int>(static_cast<unsigned char>(ch)) << std::dec
                            << std::setfill(' ');
                    } else {
                        out << ch;
                    }
            }
        }
        out << '"';
        return out.str();
    }

    static void writeJsonIntArray(std::stringstream& out, const std::vector<int>& values) {
        out << "[";
        for (size_t i = 0; i < values.size(); ++i) {
            if (i > 0)
                out << ",";
            out << values[i];
        }
        out << "]";
    }

    static void writeFileAtomic(const std::string& path, const std::string& body) {
        const auto tmpPath = path + ".tmp";

        {
            std::ofstream tmp(tmpPath, std::ios::trunc);
            if (!tmp)
                return;
            tmp << body;
            tmp.flush();
            if (!tmp)
                return;
        }

        std::rename(tmpPath.c_str(), path.c_str());
    }

    void writeDebug(const std::string& reason) const {
        const auto statePath = runtimePath("hyprland-workspace-history-state");
        const auto jsonPath  = runtimePath("hyprland-workspace-history.json");
        const auto logPath   = runtimePath("hyprland-workspace-history.log");
        if (!statePath || !jsonPath || !logPath)
            return;

        const auto revision = ++m_revision;
        const auto body = snapshotText(reason, revision);
        writeFileAtomic(*statePath, body);
        writeFileAtomic(*jsonPath, snapshotJson(reason, revision));

        std::ofstream log(*logPath, std::ios::app);
        if (log) {
            const auto now = std::time(nullptr);
            log << "--- " << std::put_time(std::localtime(&now), "%Y-%m-%d %H:%M:%S") << " ---\n";
            log << body;
        }
    }
};

CWorkspaceHistory g_workspaceHistory;

int luaCycle(lua_State* L) {
    const auto result = g_workspaceHistory.cycle(static_cast<int>(luaL_optinteger(L, 1, 1)));
    if (!result.success)
        return luaL_error(L, "%s", result.error.c_str());
    return 0;
}

int luaCommit(lua_State* L) {
    const auto result = g_workspaceHistory.commit();
    if (!result.success)
        return luaL_error(L, "%s", result.error.c_str());
    return 0;
}

int luaCancel(lua_State* L) {
    const auto result = g_workspaceHistory.cancel();
    if (!result.success)
        return luaL_error(L, "%s", result.error.c_str());
    return 0;
}

int luaDebug(lua_State*) {
    g_workspaceHistory.showDebug();
    return 0;
}

void failNotification(const std::string& reason) {
    HyprlandAPI::addNotification(PHANDLE, "[workspace-history] " + reason, CHyprColor{1.0, 0.2, 0.2, 1.0}, 5000);
}

}

APICALL EXPORT std::string PLUGIN_API_VERSION() {
    return HYPRLAND_API_VERSION;
}

APICALL EXPORT PLUGIN_DESCRIPTION_INFO PLUGIN_INIT(HANDLE handle) {
    PHANDLE = handle;

    const std::string hash       = __hyprland_api_get_hash();
    const std::string clientHash = __hyprland_api_get_client_hash();
    if (hash != clientHash) {
        failNotification("version mismatch between Hyprland headers and running Hyprland");
        throw std::runtime_error("[workspace-history] version mismatch");
    }

    static auto workspaceHook = Event::bus()->m_events.workspace.active.listen([](PHLWORKSPACE workspace) { g_workspaceHistory.observe(workspace); });
    static auto keyboardHook = Event::bus()->m_events.input.keyboard.key.listen([](IKeyboard::SKeyEvent event, Event::SCallbackInfo&) { g_workspaceHistory.onKey(event); });
    static auto startHook = Event::bus()->m_events.start.listen([] { g_workspaceHistory.seedActiveWorkspaces(); });
    static auto reloadHook = Event::bus()->m_events.config.reloaded.listen([] { g_workspaceHistory.seedActiveWorkspaces(); });
    static auto monitorHook = Event::bus()->m_events.monitor.focused.listen([](PHLMONITOR monitor) {
        if (monitor && monitor->m_activeWorkspace)
            g_workspaceHistory.observe(monitor->m_activeWorkspace);
    });

    HyprlandAPI::addLuaFunction(PHANDLE, "workspacehistory", "cycle", ::luaCycle);
    HyprlandAPI::addLuaFunction(PHANDLE, "workspacehistory", "commit", ::luaCommit);
    HyprlandAPI::addLuaFunction(PHANDLE, "workspacehistory", "cancel", ::luaCancel);
    HyprlandAPI::addLuaFunction(PHANDLE, "workspacehistory", "debug", ::luaDebug);

    g_workspaceHistory.seedActiveWorkspaces();
    HyprlandAPI::addNotification(PHANDLE, "[workspace-history] Initialized", CHyprColor{0.2, 1.0, 0.2, 1.0}, 3000);
    return {"hypr-workspace-history", "Workspace history cycling with modifier-release commits", "Ivan Malison", "0.1.0"};
}

APICALL EXPORT void PLUGIN_EXIT() {
    g_workspaceHistory.commit();
}
