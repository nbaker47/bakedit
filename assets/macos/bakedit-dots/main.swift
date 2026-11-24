// bakedit-dots-overlay: terminal-styled floating Agent Dots for macOS, bundled inside bakedit.
// Appears flush at the bottom-center of the screen when you leave bakedit (via --control-socket)
// and hides when you return. Mirrors bakedit's active theme live.
import AppKit
import Carbon
import Foundation
import SwiftUI

// MARK: - Theme (synced live from bakedit)

struct TerminalTheme: Equatable {
    var dark: Bool = true
    var bg: Color = Color(hex: "#12101a")
    var surface: Color = Color(hex: "#12101a")
    var elevated: Color = Color(hex: "#25232c")
    var border: Color = Color(hex: "#25232c")
    var accent: Color = Color(hex: "#82aaff")
    var text: Color = Color(hex: "#f8f8f2")
    var muted: Color = Color(hex: "#768d8d")
    var green: Color = Color(hex: "#50fa7b")
    var yellow: Color = Color(hex: "#f1fa8c")
    var red: Color = Color(hex: "#ff6363")
    var magenta: Color = Color(hex: "#ff00ff")
    var cyan: Color = Color(hex: "#80ffea")

    mutating func apply(json: [String: Any]) {
        if let d = json["dark"] as? Bool { dark = d }
        if let s = json["bg"] as? String { bg = Color(hex: s) }
        if let s = json["surface"] as? String { surface = Color(hex: s) }
        if let s = json["elevated"] as? String { elevated = Color(hex: s) }
        if let s = json["border"] as? String { border = Color(hex: s) }
        if let s = json["accent"] as? String { accent = Color(hex: s) }
        if let s = json["text"] as? String { text = Color(hex: s) }
        if let s = json["muted"] as? String { muted = Color(hex: s) }
        if let s = json["green"] as? String { green = Color(hex: s) }
        if let s = json["yellow"] as? String { yellow = Color(hex: s) }
        if let s = json["red"] as? String { red = Color(hex: s) }
        if let s = json["magenta"] as? String { magenta = Color(hex: s) }
        if let s = json["cyan"] as? String { cyan = Color(hex: s) }
    }
}

extension Color {
    init(hex: String) {
        let cleaned = hex.trimmingCharacters(in: CharacterSet(charactersIn: "#"))
        var val: UInt64 = 0
        Scanner(string: cleaned).scanHexInt64(&val)
        let r = Double((val >> 16) & 0xff) / 255.0
        let g = Double((val >> 8) & 0xff) / 255.0
        let b = Double(val & 0xff) / 255.0
        self.init(red: r, green: g, blue: b)
    }
}

func monoFont(size: CGFloat, weight: Font.Weight = .regular) -> Font {
    if NSFont(name: "MesloLGSDZ Nerd Font Mono", size: size) != nil {
        return .custom("MesloLGSDZ Nerd Font Mono", size: size).weight(weight)
    }
    return .system(size: size, weight: weight, design: .monospaced)
}

// MARK: - Models & Herdr CLI

struct HerdrAgent: Identifiable, Equatable, Sendable {
    let paneId: String
    let agent: String
    let status: String
    let cwd: String
    let completionSeq: Int
    let focused: Bool
    let sessionId: String?

    var id: String { paneId }
    var displayAgent: String { agent.isEmpty ? "agent" : agent }
    var shortDir: String {
        let trimmed = cwd.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        return trimmed.split(separator: "/").last.map(String.init) ?? cwd
    }
    var statusLabel: String {
        switch status {
        case "working": return "Working"
        case "blocked": return "Needs Input"
        case "idle": return "Idle"
        case "done": return "Done"
        default: return status.capitalized
        }
    }
}

enum HerdrCLI {
    static let path: String = {
        for p in ["/opt/homebrew/bin/herdr", "/usr/local/bin/herdr", NSHomeDirectory() + "/.local/bin/herdr"] {
            if FileManager.default.isExecutableFile(atPath: p) { return p }
        }
        return "/opt/homebrew/bin/herdr"
    }()

    static func run(_ args: [String], timeout: TimeInterval = 4.0) -> (Int32, String, String) {
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: path)
        proc.arguments = args
        var env = ProcessInfo.processInfo.environment
        env["PATH"] = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
        proc.environment = env
        let outP = Pipe(), errP = Pipe()
        proc.standardOutput = outP
        proc.standardError = errP
        do { try proc.run() } catch { return (-1, "", error.localizedDescription) }
        let deadline = Date().addingTimeInterval(timeout)
        while proc.isRunning && Date() < deadline { Thread.sleep(forTimeInterval: 0.02) }
        if proc.isRunning { proc.terminate(); return (-2, "", "timeout") }
        let out = String(data: outP.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        let err = String(data: errP.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        return (proc.terminationStatus, out, err)
    }

    static func listAgents() -> ([HerdrAgent], String?) {
        let (code, out, err) = run(["agent", "list"], timeout: 3.0)
        guard code == 0,
              let data = out.data(using: .utf8),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let res = json["result"] as? [String: Any],
              let raw = res["agents"] as? [[String: Any]] else {
            return ([], err.isEmpty ? "herdr offline" : err.trimmingCharacters(in: .whitespacesAndNewlines))
        }
        let agents: [HerdrAgent] = raw.compactMap { item in
            guard let paneId = item["pane_id"] as? String else { return nil }
            let sid = (item["agent_session"] as? [String: Any])?["value"] as? String
            return HerdrAgent(
                paneId: paneId,
                agent: item["agent"] as? String ?? "agent",
                status: item["agent_status"] as? String ?? "unknown",
                cwd: item["foreground_cwd"] as? String ?? (item["cwd"] as? String ?? ""),
                completionSeq: item["completion_seq"] as? Int ?? 0,
                focused: item["focused"] as? Bool ?? false,
                sessionId: sid
            )
        }
        return (agents, nil)
    }

    static func readOutput(paneId: String, lines: Int = 160) -> String {
        let (code, out, err) = run(["agent", "read", paneId, "--source", "recent", "--lines", "\(lines)", "--format", "ansi"])
        if code == 0 { return out }
        if (err + out).contains("agent_not_idle") {
            let (vc, vo, _) = run(["agent", "read", paneId, "--source", "visible", "--format", "ansi"])
            if vc == 0 { return vo }
        }
        return err.isEmpty ? "(no output)" : "Error: \(err.trimmingCharacters(in: .whitespacesAndNewlines))"
    }

    static func prompt(paneId: String, text: String) -> (Bool, String) {
        let (c, o, e) = run(["agent", "prompt", paneId, text], timeout: 6.0)
        return c == 0 ? (true, "Sent") : (false, (e + " " + o).trimmingCharacters(in: .whitespacesAndNewlines))
    }

    static func sendKeys(paneId: String, keys: [String]) -> Bool {
        run(["agent", "send-keys", paneId] + keys, timeout: 4.0).0 == 0
    }

    static func focus(paneId: String) {
        _ = run(["agent", "focus", paneId], timeout: 3.0)
    }

    static func stripANSI(_ s: String) -> String {
        var out = ""
        let sc = Array(s.unicodeScalars)
        var i = 0
        while i < sc.count {
            if sc[i].value == 0x1B, i + 1 < sc.count {
                if sc[i + 1] == "[" {
                    i += 2
                    while i < sc.count { let v = sc[i].value; i += 1; if v >= 0x40 && v <= 0x7E { break } }
                    continue
                } else if sc[i + 1] == "]" {
                    i += 2
                    while i < sc.count {
                        if sc[i].value == 0x07 { i += 1; break }
                        if sc[i].value == 0x1B && i + 1 < sc.count && sc[i + 1] == "\\" { i += 2; break }
                        i += 1
                    }
                    continue
                }
            }
            out.unicodeScalars.append(sc[i]); i += 1
        }
        return out
    }

    static func splitOutputAndStatus(_ raw: String) -> (body: String, statusLines: [String]) {
        let all = raw.components(separatedBy: .newlines)
        let plainAll = all.map { stripANSI($0) }
        var s = 0
        while s < all.count && plainAll[s].trimmingCharacters(in: .whitespaces).isEmpty { s += 1 }
        var e = all.count - 1
        while e >= s && plainAll[e].trimmingCharacters(in: .whitespaces).isEmpty { e -= 1 }
        if s > e { return ("(no output yet)", []) }
        let lines = Array(all[s...e]), plain = Array(plainAll[s...e])
        let isRule: (String) -> Bool = { let t = $0.trimmingCharacters(in: .whitespaces); return t.hasPrefix("────────") || t.hasPrefix("━━━━━━━━") }
        let isPrompt: (String) -> Bool = { let t = $0.trimmingCharacters(in: .whitespaces); return t == ">" || t == "❯" || t.hasPrefix("> ") || t.hasPrefix("❯ ") || t.hasPrefix("❯\u{00A0}") }
        var bottomRule: Int? = nil, seen = 0
        for i in stride(from: lines.count - 1, through: 0, by: -1) {
            if plain[i].trimmingCharacters(in: .whitespaces).isEmpty { continue }
            if isRule(plain[i]) { bottomRule = i; break }
            seen += 1; if seen >= 8 { break }
        }
        guard let bIdx = bottomRule else { return (lines.joined(separator: "\n"), []) }
        var topRule: Int? = nil, promptIdx: Int? = nil
        for i in stride(from: bIdx - 1, through: max(0, bIdx - 6), by: -1) {
            if plain[i].trimmingCharacters(in: .whitespaces).isEmpty { continue }
            if isRule(plain[i]) { topRule = i; break }
            if isPrompt(plain[i]) { promptIdx = i }
        }
        guard let cut = topRule ?? promptIdx else { return (lines.joined(separator: "\n"), []) }
        var status: [String] = []
        if bIdx + 1 < lines.count {
            for i in (bIdx + 1)..<lines.count where !plain[i].trimmingCharacters(in: .whitespaces).isEmpty {
                status.append(lines[i].trimmingCharacters(in: .whitespaces))
            }
        }
        var bodyEnd = cut - 1
        while bodyEnd >= 0 {
            let t = plain[bodyEnd].trimmingCharacters(in: .whitespaces)
            if t.isEmpty || t == "Jump to bottom (click) ↓" { bodyEnd -= 1 } else { break }
        }
        let body = bodyEnd < 0 ? "(no output yet)" : lines[0...bodyEnd].joined(separator: "\n")
        return (body, status)
    }

    static func readClaudeHistory(sessionId: String, maxBlocks: Int = 35) -> String? {
        let fm = FileManager.default
        let root = NSHomeDirectory() + "/.claude/projects"
        guard let subs = try? fm.contentsOfDirectory(atPath: root) else { return nil }
        guard let path = subs.map({ "\(root)/\($0)/\(sessionId).jsonl" }).first(where: { fm.fileExists(atPath: $0) }),
              let fh = FileHandle(forReadingAtPath: path) else { return nil }
        defer { try? fh.close() }
        let size = (try? fm.attributesOfItem(atPath: path)[.size] as? UInt64) ?? 0
        if size > 512 * 1024 { try? fh.seek(toOffset: size - 512 * 1024) }
        guard let raw = String(data: fh.readDataToEndOfFile(), encoding: .utf8) else { return nil }
        var lines = raw.components(separatedBy: .newlines)
        if size > 512 * 1024 && !lines.isEmpty { lines.removeFirst() }
        var blocks: [String] = []
        for l in lines where !l.isEmpty {
            guard let d = l.data(using: .utf8),
                  let obj = try? JSONSerialization.jsonObject(with: d) as? [String: Any],
                  let kind = obj["type"] as? String,
                  let msg = obj["message"] as? [String: Any] else { continue }
            if kind == "user", let t = (msg["content"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines),
               !t.isEmpty, !t.hasPrefix("This session is being continued"), !t.hasPrefix("<local-command-"), !t.hasPrefix("<command-name>") {
                blocks.append("\u{1B}[1;96m❯ \(t)\u{1B}[0m")
            } else if kind == "assistant", let items = msg["content"] as? [[String: Any]] {
                var parts: [String] = []
                for it in items {
                    if (it["type"] as? String) == "text", let txt = (it["text"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines), !txt.isEmpty {
                        parts.append(renderMarkdown(txt))
                    } else if (it["type"] as? String) == "tool_use", let name = it["name"] as? String {
                        parts.append("\u{1B}[2;36m⚙ \(name)\u{1B}[0m")
                    }
                }
                if !parts.isEmpty { blocks.append(parts.joined(separator: "\n")) }
            }
        }
        return blocks.isEmpty ? nil : blocks.suffix(maxBlocks).joined(separator: "\n\n")
    }

    static func renderMarkdown(_ md: String) -> String {
        var out: [String] = [], inCode = false
        for line in md.components(separatedBy: .newlines) {
            let t = line.trimmingCharacters(in: .whitespaces)
            if t.hasPrefix("```") { inCode.toggle(); continue }
            if inCode { out.append("  \u{1B}[94m\(line)\u{1B}[0m"); continue }
            if t.hasPrefix("#") {
                let h = t.prefix(while: { $0 == "#" }).count
                if h <= 6 && t.dropFirst(h).first == " " {
                    out.append("\u{1B}[1;93m\(renderInline(String(t.dropFirst(h + 1))))\u{1B}[0m"); continue
                }
            }
            var w = line
            if let r = w.range(of: #"^(\s*)[-*]\s+"#, options: .regularExpression) {
                let indent = w[r].prefix(while: { $0 == " " || $0 == "\t" })
                w.replaceSubrange(r, with: "\(indent)• ")
            }
            out.append(renderInline(w))
        }
        return out.joined(separator: "\n")
    }

    private static func renderInline(_ line: String) -> String {
        var s = line
        for (pat, tpl) in [
            (#"\[([^\]]+)\]\(([^)]+)\)"#, "\u{1B}[4;94m$1\u{1B}[0m"),
            (#"`([^`]+)`"#, "\u{1B}[94m$1\u{1B}[0m"),
            (#"\*\*([^*]+)\*\*"#, "\u{1B}[1;97m$1\u{1B}[0m"),
        ] {
            if let re = try? NSRegularExpression(pattern: pat) {
                s = re.stringByReplacingMatches(in: s, range: NSRange(s.startIndex..., in: s), withTemplate: tpl)
            }
        }
        return s
    }
}

// MARK: - ANSI Parser

enum ANSIParser {
    private static let p16: [(Double, Double, Double)] = [
        (0.27, 0.28, 0.35), (0.95, 0.54, 0.66), (0.65, 0.89, 0.63), (0.98, 0.89, 0.69),
        (0.54, 0.71, 0.98), (0.80, 0.65, 0.97), (0.58, 0.89, 0.84), (0.80, 0.84, 0.96),
        (0.45, 0.48, 0.58), (0.96, 0.60, 0.70), (0.71, 0.93, 0.69), (1.00, 0.92, 0.74),
        (0.60, 0.78, 1.00), (0.86, 0.73, 0.99), (0.64, 0.94, 0.89), (0.96, 0.97, 1.00),
    ]

    private static func c256(_ n: Int) -> Color {
        if n < 0 { return .white }
        if n < 16 { let (r, g, b) = p16[n]; return Color(red: r, green: g, blue: b) }
        if n < 232 {
            let i = n - 16
            let lv: [Double] = [0, 95/255, 135/255, 175/255, 215/255, 1]
            return Color(red: lv[i / 36], green: lv[(i % 36) / 6], blue: lv[i % 6])
        }
        let g = Double(8 + (n - 232) * 10) / 255.0
        return Color(red: g, green: g, blue: g)
    }

    static func parse(_ input: String, fontSize: CGFloat, defaultFg: Color) -> AttributedString {
        var result = AttributedString()
        let sc = Array(input.unicodeScalars)
        var i = 0, chunk = ""
        var fg: Color? = nil, bg: Color? = nil, bold = false, dim = false, italic = false, under = false

        func flush() {
            guard !chunk.isEmpty else { return }
            var a = AttributedString(chunk)
            var f = monoFont(size: fontSize, weight: bold ? .semibold : .regular)
            if italic { f = f.italic() }
            a.font = f
            let base = fg ?? defaultFg
            a.foregroundColor = dim ? base.opacity(0.58) : base
            if let bg { a.backgroundColor = bg }
            if under { a.underlineStyle = .single }
            result.append(a); chunk = ""
        }

        while i < sc.count {
            if sc[i].value == 0x1B, i + 1 < sc.count, sc[i + 1] == "[" {
                flush(); i += 2
                var params = "", fin: UnicodeScalar = "\0"
                while i < sc.count {
                    let s = sc[i]; i += 1
                    if s.value >= 0x40 && s.value <= 0x7E { fin = s; break }
                    params.unicodeScalars.append(s)
                }
                if fin == "m" {
                    let codes = params.isEmpty ? [0] : params.split(separator: ";").map { Int($0) ?? 0 }
                    var k = 0
                    while k < codes.count {
                        let c = codes[k]
                        switch c {
                        case 0: fg = nil; bg = nil; bold = false; dim = false; italic = false; under = false
                        case 1: bold = true
                        case 2: dim = true
                        case 3: italic = true
                        case 4: under = true
                        case 22: bold = false; dim = false
                        case 23: italic = false
                        case 24: under = false
                        case 30...37: fg = c256(c - 30)
                        case 39: fg = nil
                        case 40...47: bg = c256(c - 40)
                        case 49: bg = nil
                        case 90...97: fg = c256(c - 90 + 8)
                        case 100...107: bg = c256(c - 100 + 8)
                        case 38 where k + 2 < codes.count && codes[k + 1] == 5: fg = c256(codes[k + 2]); k += 2
                        case 38 where k + 4 < codes.count && codes[k + 1] == 2:
                            fg = Color(red: Double(codes[k+2])/255, green: Double(codes[k+3])/255, blue: Double(codes[k+4])/255); k += 4
                        case 48 where k + 2 < codes.count && codes[k + 1] == 5: bg = c256(codes[k + 2]); k += 2
                        case 48 where k + 4 < codes.count && codes[k + 1] == 2:
                            bg = Color(red: Double(codes[k+2])/255, green: Double(codes[k+3])/255, blue: Double(codes[k+4])/255); k += 4
                        default: break
                        }
                        k += 1
                    }
                }
                continue
            }
            chunk.unicodeScalars.append(sc[i]); i += 1
        }
        flush()
        return result
    }
}

// MARK: - Store

@MainActor
final class OverlayStore: ObservableObject {
    @Published var theme = TerminalTheme()
    @Published var agents: [HerdrAgent] = []
    @Published var unread: Set<String> = []
    @Published var selectedPaneId: String? = nil
    @Published var keyboardPaneId: String? = nil
    @Published var isKeyboardBarActive = false
    @Published var drawerOutput = AttributedString("")
    @Published var drawerOutputRaw = ""
    @Published var drawerStatus: [AttributedString] = []
    @Published var draft = ""
    @Published var banner: (Bool, String)? = nil
    @Published var pulseOn = false
    @Published var isAutoScroll = true
    @Published var composerFocusTrigger = 0

    private var prev: [String: HerdrAgent] = [:]
    private var seeded = false
    var onBarChanged: (() -> Void)?
    var onDrawerChanged: ((Bool) -> Void)?
    var onKeyboardBarChanged: ((Bool) -> Void)?

    var selectedAgent: HerdrAgent? { agents.first(where: { $0.paneId == selectedPaneId }) }

    func start() {
        poll()
        Timer.scheduledTimer(withTimeInterval: 1.5, repeats: true) { [weak self] _ in Task { @MainActor in self?.poll() } }
        Timer.scheduledTimer(withTimeInterval: 2.0, repeats: true) { [weak self] _ in
            Task { @MainActor in
                if let id = self?.selectedPaneId { self?.refreshDrawer(paneId: id) }
            }
        }
        Timer.scheduledTimer(withTimeInterval: 0.55, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.pulseOn.toggle() }
        }
    }

    func poll() {
        DispatchQueue.global(qos: .utility).async {
            let (list, _) = HerdrCLI.listAgents()
            Task { @MainActor in
                let changed = list != self.agents
                self.agents = list
                if self.seeded {
                    for a in list {
                        let old = self.prev[a.paneId]
                        if a.status == "working" || (a.focused && old?.focused == false) {
                            self.unread.remove(a.paneId)
                        }
                        if let old {
                            let done = (a.completionSeq > old.completionSeq && a.completionSeq > 0)
                                || (old.status == "working" && (a.status == "idle" || a.status == "done"))
                                || (old.status != "blocked" && a.status == "blocked")
                            if done && self.selectedPaneId != a.paneId && !a.focused {
                                self.unread.insert(a.paneId)
                            }
                        }
                    }
                }
                self.prev = Dictionary(uniqueKeysWithValues: list.map { ($0.paneId, $0) })
                self.seeded = true
                if changed { self.onBarChanged?() }
            }
        }
    }

    func openDrawer(for paneId: String) {
        isKeyboardBarActive = false; keyboardPaneId = nil
        if selectedPaneId != paneId { drawerOutput = AttributedString(""); drawerOutputRaw = ""; drawerStatus = [] }
        isAutoScroll = true; selectedPaneId = paneId; unread.remove(paneId); banner = nil
        onDrawerChanged?(true)
        refreshDrawer(paneId: paneId)
    }

    func closeDrawer() {
        selectedPaneId = nil; drawerStatus = []; banner = nil
        onDrawerChanged?(false)
    }

    func refreshDrawer(paneId: String) {
        let snap = agents.first(where: { $0.paneId == paneId })
        let t = theme
        DispatchQueue.global(qos: .userInitiated).async {
            let raw = HerdrCLI.readOutput(paneId: paneId)
            let parsed = HerdrCLI.splitOutputAndStatus(raw)
            var body = parsed.body
            if let a = snap, a.agent == "claude", let sid = a.sessionId, let hist = HerdrCLI.readClaudeHistory(sessionId: sid) {
                body = (a.status == "working" || a.status == "blocked") ? hist + "\n\n\u{1B}[2;33m── live pane ──\u{1B}[0m\n" + body : hist
            }
            let attrBody = ANSIParser.parse(body, fontSize: 11.5, defaultFg: t.text)
            let attrStatus = parsed.statusLines.enumerated().map { idx, l in
                ANSIParser.parse(l, fontSize: 10.5, defaultFg: idx == 0 ? t.text.opacity(0.85) : t.muted)
            }
            Task { @MainActor in
                guard self.selectedPaneId == paneId else { return }
                if self.drawerOutputRaw != body { self.drawerOutput = attrBody; self.drawerOutputRaw = body }
                self.drawerStatus = attrStatus
            }
        }
    }

    func sendDraft() {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, let id = selectedPaneId else { return }
        draft = ""; isAutoScroll = true
        DispatchQueue.global(qos: .userInitiated).async {
            let (ok, msg) = HerdrCLI.prompt(paneId: id, text: text)
            Task { @MainActor in
                self.banner = (!ok, ok ? "Sent to \(id)" : msg)
                self.poll()
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { self.refreshDrawer(paneId: id) }
            }
        }
    }

    func enterBar(prefer: String? = nil) {
        keyboardPaneId = prefer ?? agents.first(where: { unread.contains($0.paneId) })?.paneId ?? agents.first?.paneId
        isKeyboardBarActive = true
        onKeyboardBarChanged?(true)
    }

    func exitBar() {
        isKeyboardBarActive = false; keyboardPaneId = nil
        onKeyboardBarChanged?(false)
    }

    func moveSelection(_ delta: Int) {
        guard !agents.isEmpty else { return }
        let idx = agents.firstIndex(where: { $0.paneId == keyboardPaneId }) ?? 0
        keyboardPaneId = agents[(idx + delta + agents.count) % agents.count].paneId
    }
}

// MARK: - Views (Terminal Style)

struct TerminalBarView: View {
    @ObservedObject var store: OverlayStore

    var body: some View {
        let t = store.theme
        HStack(spacing: 4) {
            if store.agents.isEmpty {
                Text("○ no herdr agents")
                    .font(monoFont(size: 11))
                    .foregroundColor(t.muted)
                    .padding(.horizontal, 8)
            } else {
                ForEach(store.agents) { a in
                    let isUnread = store.unread.contains(a.paneId)
                    let isSel = store.selectedPaneId == a.paneId || (store.isKeyboardBarActive && store.keyboardPaneId == a.paneId)
                    let (glyph, col): (String, Color) = {
                        if isUnread { return ("◆", store.pulseOn ? t.magenta : t.yellow) }
                        switch a.status {
                        case "working": return (store.pulseOn ? "◐" : "◓", t.yellow)
                        case "blocked": return ("▲", t.red)
                        case "idle", "done": return ("●", t.green)
                        default: return ("○", t.muted)
                        }
                    }()
                    Button {
                        if store.selectedPaneId == a.paneId { store.closeDrawer() } else { store.openDrawer(for: a.paneId) }
                    } label: {
                        HStack(spacing: 5) {
                            Text(glyph).font(monoFont(size: 11, weight: .bold)).foregroundColor(col)
                            Text("\(a.displayAgent)·\(a.paneId)")
                                .font(monoFont(size: 11, weight: isSel || isUnread ? .semibold : .regular))
                                .foregroundColor(isSel || isUnread ? t.text : t.muted)
                        }
                        .padding(.horizontal, 8)
                        .padding(.vertical, 4)
                        .background(isSel ? t.accent.opacity(0.26) : (isUnread ? col.opacity(0.22) : t.elevated))
                        .overlay(Rectangle().strokeBorder(isSel ? t.accent : (isUnread ? col : t.border), lineWidth: 1))
                    }
                    .buttonStyle(.plain)
                }
            }
        }
        .padding(.horizontal, 6)
        .padding(.vertical, 4)
        .background(t.surface)
        .overlay(Rectangle().strokeBorder(store.isKeyboardBarActive ? t.accent : t.border, lineWidth: 1))
    }
}

struct TerminalDrawerView: View {
    @ObservedObject var store: OverlayStore
    @FocusState private var focused: Bool

    var body: some View {
        let t = store.theme
        VStack(spacing: 0) {
            // Header
            HStack(spacing: 8) {
                if let a = store.selectedAgent {
                    Text("\(a.displayAgent) · \(a.paneId)  [\(a.statusLabel)]  \(a.shortDir)")
                        .font(monoFont(size: 11.5, weight: .semibold))
                        .foregroundColor(t.text)
                        .lineLimit(1)
                }
                Spacer()
                Button("Focus") {
                    if let id = store.selectedPaneId { DispatchQueue.global().async { HerdrCLI.focus(paneId: id) } }
                }
                .font(monoFont(size: 10.5))
                .foregroundColor(t.text)
                .padding(.horizontal, 7).padding(.vertical, 3)
                .background(t.elevated).overlay(Rectangle().strokeBorder(t.border, lineWidth: 1))
                .buttonStyle(.plain)

                Button("×") { store.closeDrawer() }
                    .font(monoFont(size: 12, weight: .bold))
                    .foregroundColor(t.muted)
                    .padding(.horizontal, 6).padding(.vertical, 2)
                    .buttonStyle(.plain)
            }
            .padding(.horizontal, 10).padding(.vertical, 7)
            .background(t.surface)

            Rectangle().fill(t.border).frame(height: 1)

            // Transcript
            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 0) {
                        Text(store.drawerOutput)
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(10)
                        Color.clear.frame(height: 1).id("BOTTOM")
                    }
                }
                .background(t.bg)
                .onChange(of: store.drawerOutputRaw) { _, _ in if store.isAutoScroll { proxy.scrollTo("BOTTOM", anchor: .bottom) } }
                .onChange(of: store.selectedPaneId) { _, _ in proxy.scrollTo("BOTTOM", anchor: .bottom) }
                .onAppear { proxy.scrollTo("BOTTOM", anchor: .bottom) }
            }

            if !store.drawerStatus.isEmpty {
                Rectangle().fill(t.border).frame(height: 1)
                VStack(alignment: .leading, spacing: 1) {
                    ForEach(Array(store.drawerStatus.enumerated()), id: \.offset) { _, line in
                        Text(line).lineLimit(1).truncationMode(.tail).frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                .padding(.horizontal, 10).padding(.vertical, 5)
                .background(t.surface)
            }

            Rectangle().fill(t.border).frame(height: 1)

            // Composer
            HStack(alignment: .bottom, spacing: 6) {
                Text(">").font(monoFont(size: 12, weight: .bold)).foregroundColor(t.accent).padding(.bottom, 5)
                TextField("Send message (↓ bar, Esc close)...", text: $store.draft, axis: .vertical)
                    .textFieldStyle(.plain)
                    .font(monoFont(size: 12))
                    .foregroundColor(t.text)
                    .lineLimit(1...5)
                    .padding(.vertical, 4)
                    .focused($focused)
                    .onSubmit { store.sendDraft() }
                Button("Send") { store.sendDraft() }
                    .font(monoFont(size: 11, weight: .semibold))
                    .foregroundColor(t.bg)
                    .padding(.horizontal, 10).padding(.vertical, 4)
                    .background(t.accent)
                    .buttonStyle(.plain)
            }
            .padding(.horizontal, 10).padding(.vertical, 6)
            .background(t.elevated)
        }
        .frame(width: 640, height: 450)
        .background(t.bg)
        .overlay(Rectangle().strokeBorder(t.accent, lineWidth: 1))
        .onAppear { DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) { focused = true } }
        .onChange(of: store.composerFocusTrigger) { _, _ in DispatchQueue.main.asyncAfter(deadline: .now() + 0.04) { focused = true } }
    }
}

// MARK: - Panels & AppDelegate

final class BarPanel: NSPanel {
    var allowsKey = false
    var onLeft: (() -> Void)?, onRight: (() -> Void)?, onUp: (() -> Void)?, onConfirm: (() -> Void)?, onEscape: (() -> Void)?
    override var canBecomeKey: Bool { allowsKey }
    override var canBecomeMain: Bool { false }
    override func keyDown(with e: NSEvent) {
        switch e.keyCode {
        case 123: onLeft?()
        case 124, 125: onRight?()
        case 126: onUp?()
        case 36, 49, 76: onConfirm?()
        case 53: onEscape?()
        default:
            if e.charactersIgnoringModifiers?.lowercased() == "h" { onLeft?() }
            else if e.charactersIgnoringModifiers?.lowercased() == "l" { onRight?() }
            else { super.keyDown(with: e) }
        }
    }
}

final class DrawerPanel: NSPanel {
    var onEscape: (() -> Void)?, onDown: (() -> Void)?, onScrollChanged: ((Bool) -> Void)?
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }

    override func sendEvent(_ event: NSEvent) {
        if event.type == .scrollWheel {
            super.sendEvent(event)
            DispatchQueue.main.async { [weak self] in if let self { self.onScrollChanged?(self.nearBottom()) } }
            return
        }
        if event.type == .keyDown {
            let m = event.modifierFlags.intersection([.command, .control, .option, .shift])
            if event.keyCode == 53 && m.isEmpty { onEscape?(); return }
            if (event.keyCode == 125 || event.keyCode == 126) && (m == .shift || m == [.shift, .command]) {
                scrollOutput(up: event.keyCode == 126, delta: m.contains(.command) ? 144 : 48)
                return
            }
            if event.keyCode == 125 && m.isEmpty { onDown?(); return }
        }
        super.sendEvent(event)
    }

    private func scrollView() -> NSScrollView? {
        var best: NSScrollView? = nil, area: CGFloat = 0
        func walk(_ v: NSView) {
            if let sv = v as? NSScrollView, sv.frame.width * sv.frame.height > area { area = sv.frame.width * sv.frame.height; best = sv }
            v.subviews.forEach(walk)
        }
        if let root = contentView { walk(root) }
        return best
    }

    private func nearBottom() -> Bool {
        guard let sv = scrollView(), let doc = sv.documentView else { return true }
        let clip = sv.contentView
        if doc.frame.height <= clip.bounds.height { return true }
        return doc.isFlipped ? (doc.frame.height - (clip.bounds.origin.y + clip.bounds.height)) <= 32 : clip.bounds.origin.y <= 32
    }

    private func scrollOutput(up: Bool, delta: CGFloat) {
        guard let sv = scrollView(), let doc = sv.documentView else { return }
        let clip = sv.contentView
        let maxY = max(0, doc.frame.height - clip.bounds.height)
        let d = doc.isFlipped ? (up ? -delta : delta) : (up ? delta : -delta)
        let ny = max(0, min(maxY, clip.bounds.origin.y + d))
        clip.scroll(to: NSPoint(x: 0, y: ny))
        sv.reflectScrolledClipView(clip)
        onScrollChanged?(up ? false : nearBottom())
    }
}

@MainActor
final class OverlayAppDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate {
    let store = OverlayStore()
    var bar: BarPanel!
    var barHost: NSHostingView<TerminalBarView>!
    var drawer: DrawerPanel!
    var lastApp: NSRunningApplication?
    var barVisible = true
    var suppressRestore = false

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        setupEditMenu()
        let args = CommandLine.arguments
        if args.contains("--start-hidden") { barVisible = false }

        NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didActivateApplicationNotification, object: nil, queue: .main
        ) { [weak self] n in
            if let app = n.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication,
               app.bundleIdentifier != Bundle.main.bundleIdentifier {
                Task { @MainActor in self?.lastApp = app }
            }
        }

        bar = BarPanel(contentRect: NSRect(x: 0, y: 0, width: 320, height: 32), styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        bar.level = .floating; bar.hidesOnDeactivate = false; bar.isOpaque = false; bar.backgroundColor = .clear
        bar.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary]
        bar.delegate = self
        bar.onLeft = { [weak self] in self?.store.moveSelection(-1) }
        bar.onRight = { [weak self] in self?.store.moveSelection(1) }
        bar.onUp = { [weak self] in if let id = self?.store.keyboardPaneId { self?.store.openDrawer(for: id); self?.store.composerFocusTrigger += 1 } }
        bar.onConfirm = { [weak self] in if let id = self?.store.keyboardPaneId { self?.store.openDrawer(for: id) } }
        bar.onEscape = { [weak self] in self?.store.exitBar(); self?.restoreApp() }
        barHost = NSHostingView(rootView: TerminalBarView(store: store))
        bar.contentView = barHost

        drawer = DrawerPanel(contentRect: NSRect(x: 0, y: 0, width: 640, height: 450), styleMask: [.borderless], backing: .buffered, defer: false)
        drawer.level = .floating; drawer.hidesOnDeactivate = false; drawer.isOpaque = false; drawer.backgroundColor = .clear
        drawer.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        drawer.onEscape = { [weak self] in self?.store.closeDrawer() }
        drawer.onDown = { [weak self] in
            guard let self else { return }
            let cur = self.store.selectedPaneId
            self.suppressRestore = true
            self.store.closeDrawer()
            self.store.enterBar(prefer: cur)
        }
        drawer.onScrollChanged = { [weak self] atBottom in self?.store.isAutoScroll = atBottom }
        drawer.contentView = NSHostingView(rootView: TerminalDrawerView(store: store))

        store.onBarChanged = { [weak self] in DispatchQueue.main.async { self?.centerBar() } }
        store.onDrawerChanged = { [weak self] show in self?.setDrawerVisible(show) }
        store.onKeyboardBarChanged = { [weak self] active in
            guard let self else { return }
            self.bar.allowsKey = active
            if active { NSApp.activate(ignoringOtherApps: true); self.bar.makeKeyAndOrderFront(nil) }
            else { self.bar.resignKey() }
        }

        store.start()
        centerBar()
        if barVisible { bar.orderFrontRegardless() }

        if let idx = args.firstIndex(of: "--control-socket"), idx + 1 < args.count {
            startControlSocket(path: args[idx + 1])
        }
    }

    private func setupEditMenu() {
        let m = NSMenu(), item = NSMenuItem(), edit = NSMenu(title: "Edit")
        edit.addItem(withTitle: "Cut", action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        edit.addItem(withTitle: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        edit.addItem(withTitle: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
        edit.addItem(withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        item.submenu = edit; m.addItem(item); NSApp.mainMenu = m
    }

    func windowDidResize(_ notification: Notification) { centerBar() }

    private func centerBar() {
        guard let screen = NSScreen.main ?? NSScreen.screens.first else { return }
        let fit = barHost.fittingSize
        let w = max(140, fit.width), h = max(30, fit.height)
        let x = screen.frame.midX - w / 2
        let y = screen.frame.minY
        bar.setFrame(NSRect(x: x, y: y, width: w, height: h), display: true)
    }

    private func setDrawerVisible(_ show: Bool) {
        if show {
            let bf = bar.frame
            let screen = bar.screen ?? NSScreen.main ?? NSScreen.screens.first
            let vis = screen?.visibleFrame ?? NSRect(x: 0, y: 0, width: 1440, height: 900)
            let x = max(vis.minX + 8, min(bf.midX - drawer.frame.width / 2, vis.maxX - drawer.frame.width - 8))
            let y = bf.maxY + 6
            drawer.setFrameOrigin(NSPoint(x: x, y: y))
            NSApp.activate(ignoringOtherApps: true)
            drawer.makeKeyAndOrderFront(nil)
        } else {
            drawer.orderOut(nil)
            if suppressRestore { suppressRestore = false } else { restoreApp() }
        }
    }

    private func restoreApp() {
        if let app = lastApp, !app.isTerminated {
            NSApp.yieldActivation(to: app)
            app.activate(options: [])
        }
        if barVisible { bar.orderFrontRegardless() }
    }

    private func startControlSocket(path: String) {
        DispatchQueue.global(qos: .utility).async {
            unlink(path)
            let fd = socket(AF_UNIX, SOCK_STREAM, 0)
            guard fd >= 0 else { return }
            var addr = sockaddr_un()
            addr.sun_family = sa_family_t(AF_UNIX)
            let bytes = Array(path.utf8)
            guard bytes.count < MemoryLayout.size(ofValue: addr.sun_path) else { close(fd); return }
            withUnsafeMutablePointer(to: &addr.sun_path) { ptr in
                ptr.withMemoryRebound(to: UInt8.self, capacity: bytes.count + 1) { dst in
                    for (i, b) in bytes.enumerated() { dst[i] = b }
                    dst[bytes.count] = 0
                }
            }
            let ok = withUnsafePointer(to: &addr) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    Darwin.bind(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
                }
            }
            guard ok == 0, listen(fd, 8) == 0 else { close(fd); return }
            while true {
                let cfd = accept(fd, nil, nil)
                guard cfd >= 0 else { continue }
                var buf = [UInt8](repeating: 0, count: 4096)
                let n = read(cfd, &buf, buf.count)
                close(cfd)
                if n > 0, let str = String(bytes: buf[0..<n], encoding: .utf8) {
                    for line in str.components(separatedBy: .newlines) where !line.isEmpty {
                        if let d = line.data(using: .utf8), let obj = try? JSONSerialization.jsonObject(with: d) as? [String: Any] {
                            Task { @MainActor in self.handleControl(obj) }
                        }
                    }
                }
            }
        }
    }

    private func handleControl(_ obj: [String: Any]) {
        switch obj["cmd"] as? String {
        case "show":
            barVisible = true
            centerBar()
            bar.orderFrontRegardless()
        case "hide":
            barVisible = false
            store.closeDrawer()
            bar.orderOut(nil)
        case "theme":
            store.theme.apply(json: obj)
        case "quit":
            NSApp.terminate(nil)
        default:
            break
        }
    }
}

@main
struct BakeditDotsOverlayMain {
    static func main() {
        let app = NSApplication.shared
        let delegate = OverlayAppDelegate()
        app.delegate = delegate
        app.run()
    }
}
