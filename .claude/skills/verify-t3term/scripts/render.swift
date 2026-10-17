// render.swift
//
// The renderer behind `t3v render`, in the verify-t3term skill.
//
// It turns one pane captured with `tmux capture-pane -p -e` into a PNG. The image holds the
// captured cells and nothing else: each cell's character, SGR colors and attributes on a fixed
// monospace grid. It adds no text, no window frame and no cursor (capture-pane doesn't record
// the cursor). It is not a screenshot of a terminal app.
//
// How t3v builds and runs it:
// - `t3v render` builds this file with `swiftc -O` into /tmp/t3v-bin/render-HEX, where HEX is the
//   first 12 hex digits of this file's sha256. It builds once per version of the file and reuses
//   the build after. swiftc runs from /tmp/t3v-bin, so nothing it writes lands in the repository.
// - It then runs `/tmp/t3v-bin/render-HEX IN.ans OUT.png --cols N --rows N`, with the width and
//   height the capture recorded. IN is a copy in a scratch folder, since this tool writes OUT only
//   outside the folder IN is in.
// - The usage text below still calls the tool t3term-provider-render, the name it was first
//   built under.
//
// How it reads a capture:
// - Only SGR sequences change anything. A capture holds styled rows, not cursor movement, so
//   other escape sequences and control characters are skipped and counted on stderr.
// - SGR state carries from one line to the next. capture-pane writes a code only when a cell's
//   style differs from the cell before it, and that includes the last cell of the line above.
// - capture-pane trims trailing spaces. Each trimmed space had the style active at the end of
//   the line, because a style change would have left a code there and stopped the trim. The rest
//   of the row is filled with that style.
// - Widths follow tmux 3.6. East Asian wide and fullwidth characters and emoji take two columns
//   and ambiguous ones take one. Combining and format characters join the cell before them. An
//   emoji sequence (ZWJ, skin tone, keycap, flag or VS16) is one cell two columns wide. A row
//   wider than --cols means these widths disagree with tmux, so the tool stops rather than draw
//   a skewed grid.
// - Glyphs come from the chosen font, with CoreText's system fallback for CJK and emoji. A glyph
//   wider than its cells is scaled down to fit them. Box-drawing and block characters are drawn
//   as lines and rectangles so borders join, as most terminals do. --font-boxes turns that off.

import AppKit
import CoreGraphics
import CoreText
import Foundation
import ImageIO

// MARK: - Colors and styles

struct RGB: Equatable {
    var r: UInt8
    var g: UInt8
    var b: UInt8

    init(_ r: UInt8, _ g: UInt8, _ b: UInt8) {
        self.r = r
        self.g = g
        self.b = b
    }

    init(hex: UInt32) {
        self.init(UInt8((hex >> 16) & 0xff), UInt8((hex >> 8) & 0xff), UInt8(hex & 0xff))
    }

    /// This color moved `amount` of the way toward `other`.
    func mixed(with other: RGB, _ amount: Double) -> RGB {
        func mix(_ a: UInt8, _ b: UInt8) -> UInt8 {
            UInt8((Double(a) * (1 - amount) + Double(b) * amount).rounded())
        }
        return RGB(mix(r, other.r), mix(g, other.g), mix(b, other.b))
    }
}

enum TermColor: Equatable {
    /// SGR 39 or 49: the terminal's own color.
    case unset
    case indexed(Int)
    case rgb(RGB)
}

struct Style: Equatable {
    var fg = TermColor.unset
    var bg = TermColor.unset
    var bold = false
    var dim = false
    var italic = false
    var underline = false
    var inverse = false
    var hidden = false
    var strike = false
}

struct Cell {
    /// One grapheme cluster, or "" for the second column of a wide one.
    var text: String
    /// Columns the text covers, or 0 for the second column of a wide one.
    var width: Int
    var style: Style
}

let sRGB = CGColorSpace(name: CGColorSpace.sRGB)!

func cgColor(_ color: RGB) -> CGColor {
    let parts: [CGFloat] = [CGFloat(color.r) / 255, CGFloat(color.g) / 255, CGFloat(color.b) / 255, 1]
    return CGColor(colorSpace: sRGB, components: parts)!
}

/// xterm's 256-color palette. Terminal themes often change the first 16.
func xterm256(_ index: Int) -> RGB {
    let n = max(0, min(255, index))
    if n < 16 {
        let base: [UInt32] = [
            0x000000, 0xcd0000, 0x00cd00, 0xcdcd00, 0x0000ee, 0xcd00cd, 0x00cdcd, 0xe5e5e5,
            0x7f7f7f, 0xff0000, 0x00ff00, 0xffff00, 0x5c5cff, 0xff00ff, 0x00ffff, 0xffffff,
        ]
        return RGB(hex: base[n])
    }
    if n < 232 {
        let levels: [UInt8] = [0, 95, 135, 175, 215, 255]
        let i = n - 16
        return RGB(levels[i / 36], levels[(i / 6) % 6], levels[i % 6])
    }
    let gray = UInt8(8 + (n - 232) * 10)
    return RGB(gray, gray, gray)
}

/// The color in the arguments of SGR 38 or 48: [code, 5, n], [code, 2, r, g, b], or with colons
/// [code, 2, colorspace, r, g, b]. A missing number is -1.
func extendedColor(_ p: [Int], colonForm: Bool) -> TermColor? {
    guard p.count >= 2 else { return nil }
    if p[1] == 5 {
        guard p.count >= 3, p[2] >= 0 else { return nil }
        return .indexed(min(p[2], 255))
    }
    if p[1] == 2 {
        let start = colonForm && p.count >= 6 ? 3 : 2
        guard p.count >= start + 3 else { return nil }
        let parts = Array(p[start..<(start + 3)])
        guard parts.allSatisfy({ $0 >= 0 }) else { return nil }
        return .rgb(RGB(UInt8(clamping: parts[0]), UInt8(clamping: parts[1]), UInt8(clamping: parts[2])))
    }
    return nil
}

func applySGR(_ parameters: String, to style: inout Style) {
    let fields = parameters.split(separator: ";", omittingEmptySubsequences: false).map(String.init)
    var i = 0
    while i < fields.count {
        let field = fields[i]
        if field.contains(":") {
            // Colon form, such as 38:2::r:g:b or 4:3.
            let parts = field.split(separator: ":", omittingEmptySubsequences: false).map { Int($0) ?? -1 }
            switch parts[0] {
            case 38:
                if let color = extendedColor(parts, colonForm: true) { style.fg = color }
            case 48:
                if let color = extendedColor(parts, colonForm: true) { style.bg = color }
            case 4:
                style.underline = parts.count < 2 || parts[1] != 0
            default:
                break
            }
            i += 1
            continue
        }
        // An empty field means 0.
        let code = Int(field) ?? 0
        switch code {
        case 0: style = Style()
        case 1: style.bold = true
        case 2: style.dim = true
        case 3: style.italic = true
        case 4, 21: style.underline = true
        case 7: style.inverse = true
        case 8: style.hidden = true
        case 9: style.strike = true
        case 22:
            style.bold = false
            style.dim = false
        case 23: style.italic = false
        case 24: style.underline = false
        case 27: style.inverse = false
        case 28: style.hidden = false
        case 29: style.strike = false
        case 30...37: style.fg = .indexed(code - 30)
        case 39: style.fg = .unset
        case 40...47: style.bg = .indexed(code - 40)
        case 49: style.bg = .unset
        case 90...97: style.fg = .indexed(code - 90 + 8)
        case 100...107: style.bg = .indexed(code - 100 + 8)
        case 38, 48, 58:
            let rest = fields[(i + 1)...].prefix(4).map { Int($0) ?? -1 }
            let used = rest.first == 5 ? 2 : (rest.first == 2 ? 4 : 0)
            // 58 is the underline color, which this tool doesn't draw.
            if code != 58, let color = extendedColor([code] + rest, colonForm: false) {
                if code == 38 { style.fg = color } else { style.bg = color }
            }
            i += used
        default:
            break
        }
        i += 1
    }
}

// MARK: - Widths

/// East Asian Wide and Fullwidth ranges outside emoji. `isEmojiPresentation` covers the emoji.
func isWide(_ value: UInt32) -> Bool {
    switch value {
    case 0x1100...0x115F, 0x2329...0x232A, 0x2E80...0x303E, 0x3041...0x33FF, 0x3400...0x4DBF,
         0x4E00...0x9FFF, 0xA000...0xA4CF, 0xA960...0xA97F, 0xAC00...0xD7A3, 0xF900...0xFAFF,
         0xFE10...0xFE19, 0xFE30...0xFE6F, 0xFF00...0xFF60, 0xFFE0...0xFFE6,
         0x16FE0...0x16FE4, 0x17000...0x18CFF, 0x1B000...0x1B2FF, 0x1F200...0x1F2FF,
         0x20000...0x2FFFD, 0x30000...0x3FFFD:
        return true
    default:
        return false
    }
}

func isZeroWidth(_ scalar: Unicode.Scalar) -> Bool {
    let value = scalar.value
    // A soft hyphen takes a column, as in wcwidth.
    if value == 0xAD { return false }
    // Hangul medial vowels and final consonants join the syllable before them.
    if (0x1160...0x11FF).contains(value) || (0xD7B0...0xD7FF).contains(value) { return true }
    switch scalar.properties.generalCategory {
    case .nonspacingMark, .enclosingMark, .format:
        return true
    default:
        return false
    }
}

func scalarWidth(_ scalar: Unicode.Scalar) -> Int {
    if isZeroWidth(scalar) { return 0 }
    if scalar.properties.isEmojiPresentation || isWide(scalar.value) { return 2 }
    return 1
}

/// Columns tmux gives one grapheme cluster.
func clusterWidth(_ character: Character) -> Int {
    let scalars = Array(character.unicodeScalars)
    if scalars.count == 1 { return scalarWidth(scalars[0]) }
    let total = scalars.reduce(0) { $0 + scalarWidth($1) }
    let hasEmoji = scalars.contains {
        $0.properties.isEmojiPresentation || ($0.properties.isEmoji && $0.value >= 0x2000)
    }
    // tmux 3.4 and later keep an emoji sequence in one cell two columns wide.
    let emojiSequence = scalars.contains { scalar in
        switch scalar.value {
        case 0xFE0F, 0x20E3, 0x1F3FB...0x1F3FF, 0x1F1E6...0x1F1FF:
            return true
        case 0x200D:
            return hasEmoji
        default:
            return false
        }
    }
    return emojiSequence && total > 0 ? 2 : total
}

// MARK: - Parsing

final class Parser {
    private enum Mode { case text, escape, escapeIntermediate, csi, string, stringEscape }

    let cols: Int
    private(set) var rows: [[Cell]] = []
    private(set) var overflow: [(row: Int, width: Int)] = []
    private(set) var skippedSequences = 0
    private(set) var skippedCharacters = 0

    private var mode = Mode.text
    private var style = Style()
    private var line: [Cell] = []
    private var pending = ""
    private var csi = ""

    init(cols: Int) {
        self.cols = cols
    }

    func read(_ text: String) {
        for scalar in text.unicodeScalars {
            take(scalar)
        }
        finishText()
        if !line.isEmpty { endLine() }
    }

    private func take(_ scalar: Unicode.Scalar) {
        let value = scalar.value
        switch mode {
        case .text:
            switch value {
            case 0x1B:
                finishText()
                mode = .escape
            case 0x0A:
                endLine()
            case 0x00..<0x20, 0x7F, 0x80..<0xA0:
                skippedCharacters += 1
            default:
                pending.unicodeScalars.append(scalar)
            }
        case .escape:
            switch value {
            case 0x5B:
                csi = ""
                mode = .csi
            case 0x5D, 0x50, 0x58, 0x5E, 0x5F:
                // OSC, DCS, SOS, PM and APC run until a string terminator.
                skippedSequences += 1
                mode = .string
            case 0x20..<0x30:
                mode = .escapeIntermediate
            default:
                skippedSequences += 1
                mode = .text
            }
        case .escapeIntermediate:
            if (0x30..<0x7F).contains(value) {
                skippedSequences += 1
                mode = .text
            } else if !(0x20..<0x30).contains(value) {
                skippedSequences += 1
                mode = .text
                take(scalar)
            }
        case .csi:
            if (0x40..<0x7F).contains(value) {
                if value == 0x6D, csi.unicodeScalars.allSatisfy({ (0x30...0x3B).contains($0.value) }) {
                    applySGR(csi, to: &style)
                } else {
                    skippedSequences += 1
                }
                mode = .text
            } else if (0x20..<0x40).contains(value) {
                csi.unicodeScalars.append(scalar)
            } else {
                // A broken sequence: drop it and read this character as text.
                skippedSequences += 1
                mode = .text
                take(scalar)
            }
        case .string:
            if value == 0x07 || value == 0x9C {
                mode = .text
            } else if value == 0x1B {
                mode = .stringEscape
            }
        case .stringEscape:
            if value == 0x5C {
                mode = .text
            } else {
                mode = .escape
                take(scalar)
            }
        }
    }

    /// Lays the text read since the last escape sequence into cells with the current style.
    private func finishText() {
        guard !pending.isEmpty else { return }
        for character in pending {
            let width = clusterWidth(character)
            if width == 0 {
                // tmux adds a zero-width character to the cell before it.
                if let last = line.lastIndex(where: { $0.width > 0 }) {
                    line[last].text.append(character)
                } else {
                    skippedCharacters += 1
                }
                continue
            }
            line.append(Cell(text: String(character), width: width, style: style))
            for _ in 1..<width {
                line.append(Cell(text: "", width: 0, style: style))
            }
        }
        pending = ""
    }

    private func endLine() {
        finishText()
        if line.count > cols {
            overflow.append((row: rows.count, width: line.count))
            line.removeSubrange(cols...)
        }
        // The spaces capture-pane trimmed had the style active now.
        while line.count < cols {
            line.append(Cell(text: " ", width: 1, style: style))
        }
        rows.append(line)
        line = []
    }
}

// MARK: - Options

struct Options {
    var input = ""
    var output = ""
    var cols = 0
    var rows: Int?
    var font = "Menlo"
    var fontSize: CGFloat = 14
    var scale: CGFloat = 2
    var cellWidth: CGFloat?
    var cellHeight: CGFloat?
    var padding: CGFloat = 0
    // SGR 39 and 49 ask for the terminal's own colors. t3term's theme (src/tui/theme.rs) paints
    // its canvas 0a0a0a with f5f5f5 text, so those stand in for them.
    var defaultFG = RGB(hex: 0xf5f5f5)
    var defaultBG = RGB(hex: 0x0a0a0a)
    var fontBoxes = false
    var allowOverflow = false
}

func fail(_ message: String, code: Int32 = 1) -> Never {
    FileHandle.standardError.write(Data("t3term-provider-render: \(message)\n".utf8))
    exit(code)
}

func warn(_ message: String) {
    FileHandle.standardError.write(Data("t3term-provider-render: \(message)\n".utf8))
}

func usage() -> Never {
    let text = """
    usage: t3term-provider-render IN.ans OUT.png --cols N [options]

    IN.ans is the output of `tmux capture-pane -p -e`. OUT.png holds the captured cells and
    nothing else.

      --cols N           pane width in cells (required, since capture-pane trims trailing spaces)
      --rows N           pane height in cells (default: the number of lines in IN.ans)
      --font NAME        installed font name, or system-mono for SF Mono (default Menlo)
      --font-size PT     font size in points (default 14)
      --scale N          pixels per point (default 2)
      --cell-width PT    cell width in points (default: the font's advance for M)
      --cell-height PT   cell height in points (default: the font's ascent + descent + leading)
      --padding PT       margin filled with the default background (default 0)
      --default-fg HEX   color for SGR 39 (default f5f5f5, t3term's theme text)
      --default-bg HEX   color for SGR 49 (default 0a0a0a, t3term's theme canvas)
      --font-boxes       draw box-drawing and block characters with the font
      --allow-overflow   clip rows wider than --cols instead of stopping

    """
    FileHandle.standardError.write(Data(text.utf8))
    exit(64)
}

func parseOptions() -> Options {
    var options = Options()
    var positional: [String] = []
    let args = Array(CommandLine.arguments.dropFirst())
    var index = 0

    func value(_ flag: String) -> String {
        index += 1
        guard index < args.count else { fail("\(flag) needs a value", code: 64) }
        return args[index]
    }
    func number(_ flag: String, allowZero: Bool = false) -> CGFloat {
        let text = value(flag)
        guard let number = Double(text), number > 0 || (allowZero && number == 0) else {
            fail("\(flag) needs a positive number, not \(text)", code: 64)
        }
        return CGFloat(number)
    }
    func count(_ flag: String) -> Int {
        let text = value(flag)
        guard let number = Int(text), number > 0 else {
            fail("\(flag) needs a positive whole number, not \(text)", code: 64)
        }
        return number
    }
    func color(_ flag: String) -> RGB {
        var text = value(flag)
        if text.hasPrefix("#") { text.removeFirst() }
        guard text.count == 6, let hex = UInt32(text, radix: 16) else {
            fail("\(flag) needs a six-digit hex color, not \(text)", code: 64)
        }
        return RGB(hex: hex)
    }

    while index < args.count {
        let arg = args[index]
        switch arg {
        case "--cols": options.cols = count(arg)
        case "--rows": options.rows = count(arg)
        case "--font": options.font = value(arg)
        case "--font-size": options.fontSize = number(arg)
        case "--scale": options.scale = number(arg)
        case "--cell-width": options.cellWidth = number(arg)
        case "--cell-height": options.cellHeight = number(arg)
        case "--padding": options.padding = number(arg, allowZero: true)
        case "--default-fg": options.defaultFG = color(arg)
        case "--default-bg": options.defaultBG = color(arg)
        case "--font-boxes": options.fontBoxes = true
        case "--allow-overflow": options.allowOverflow = true
        case "-h", "--help": usage()
        default:
            if arg.hasPrefix("-") { fail("unknown option \(arg)", code: 64) }
            positional.append(arg)
        }
        index += 1
    }
    guard positional.count == 2 else { usage() }
    guard options.cols > 0 else { fail("--cols is required", code: 64) }
    options.input = positional[0]
    options.output = positional[1]
    return options
}

// MARK: - Drawing

final class Fonts {
    let regular: CTFont
    private var variants: [Int: CTFont] = [:]

    init(name: String, size: CGFloat) {
        if name == "system-mono" {
            regular = NSFont.monospacedSystemFont(ofSize: size, weight: .regular) as CTFont
            return
        }
        let font = CTFontCreateWithName(name as CFString, size, nil)
        // CoreText puts a default font in place of a name it doesn't know. Refuse that.
        let postScript = CTFontCopyPostScriptName(font) as String
        let family = CTFontCopyFamilyName(font) as String
        guard postScript == name || family == name else {
            fail("font \(name) isn't installed; CoreText offered \(postScript)")
        }
        regular = font
    }

    func font(bold: Bool, italic: Bool) -> CTFont {
        let key = (bold ? 1 : 0) | (italic ? 2 : 0)
        if key == 0 { return regular }
        if let cached = variants[key] { return cached }
        var traits = CTFontSymbolicTraits()
        if bold { traits.insert(.traitBold) }
        if italic { traits.insert(.traitItalic) }
        let font = CTFontCreateCopyWithSymbolicTraits(regular, 0, nil, traits, traits) ?? regular
        variants[key] = font
        return font
    }
}

/// Sizes in pixels.
struct Metrics {
    let cellWidth: CGFloat
    let cellHeight: CGFloat
    /// Height of the baseline above the bottom of a cell.
    let baseline: CGFloat
    /// Thickness of box-drawing lines, underlines and strikethroughs.
    let lineWidth: CGFloat
    let underlineDepth: CGFloat
    let strikeHeight: CGFloat

    init(font: CTFont, options: Options) {
        let ascent = CTFontGetAscent(font)
        let descent = CTFontGetDescent(font)
        let leading = CTFontGetLeading(font)
        var character: UniChar = 0x4D
        var glyph = CGGlyph(0)
        CTFontGetGlyphsForCharacters(font, &character, &glyph, 1)
        let advance = CGFloat(CTFontGetAdvancesForGlyphs(font, .horizontal, &glyph, nil, 1))
        let natural = ascent + descent + leading
        cellWidth = max(1, (options.cellWidth.map { $0 * options.scale } ?? advance).rounded(.up))
        cellHeight = max(1, (options.cellHeight.map { $0 * options.scale } ?? natural).rounded(.up))
        baseline = (descent + leading / 2 + (cellHeight - natural) / 2).rounded()
        lineWidth = max(1, CTFontGetUnderlineThickness(font).rounded())
        underlineDepth = max(lineWidth, (-CTFontGetUnderlinePosition(font)).rounded())
        strikeHeight = (CTFontGetXHeight(font) / 2).rounded()
    }
}

func cellColors(_ style: Style, _ options: Options) -> (fg: RGB, bg: RGB) {
    func resolve(_ color: TermColor, _ fallback: RGB) -> RGB {
        switch color {
        case .unset: return fallback
        case .indexed(let index): return xterm256(index)
        case .rgb(let rgb): return rgb
        }
    }
    var fg = resolve(style.fg, options.defaultFG)
    var bg = resolve(style.bg, options.defaultBG)
    if style.inverse { swap(&fg, &bg) }
    if style.dim { fg = fg.mixed(with: bg, 0.5) }
    return (fg, bg)
}

/// Light box-drawing characters as the cell edges they reach: l, r, u and d. ~ marks a rounded
/// corner.
let boxEdges: [UInt32: String] = [
    0x2500: "lr", 0x2502: "ud", 0x250C: "rd", 0x2510: "ld", 0x2514: "ru", 0x2518: "lu",
    0x251C: "rud", 0x2524: "lud", 0x252C: "lrd", 0x2534: "lru", 0x253C: "lrud",
    0x2574: "l", 0x2575: "u", 0x2576: "r", 0x2577: "d",
    0x256D: "rd~", 0x256E: "ld~", 0x256F: "lu~", 0x2570: "ru~",
]

/// The filled part of a block element, in a cell whose y grows upward.
func blockRect(_ value: UInt32, in rect: CGRect) -> CGRect? {
    func eighths(_ n: UInt32, of length: CGFloat) -> CGFloat {
        (length * CGFloat(n) / 8).rounded()
    }
    switch value {
    case 0x2580:
        let part = eighths(4, of: rect.height)
        return CGRect(x: rect.minX, y: rect.maxY - part, width: rect.width, height: part)
    case 0x2581...0x2588:
        return CGRect(x: rect.minX, y: rect.minY, width: rect.width, height: eighths(value - 0x2580, of: rect.height))
    case 0x2589...0x258F:
        return CGRect(x: rect.minX, y: rect.minY, width: eighths(0x2590 - value, of: rect.width), height: rect.height)
    case 0x2590:
        let part = eighths(4, of: rect.width)
        return CGRect(x: rect.maxX - part, y: rect.minY, width: part, height: rect.height)
    case 0x2594:
        let part = eighths(1, of: rect.height)
        return CGRect(x: rect.minX, y: rect.maxY - part, width: rect.width, height: part)
    case 0x2595:
        let part = eighths(1, of: rect.width)
        return CGRect(x: rect.maxX - part, y: rect.minY, width: part, height: rect.height)
    default:
        return nil
    }
}

/// Draws a box-drawing or block character as shapes. Returns false for any other text.
func drawShape(_ text: String, in rect: CGRect, color: CGColor, metrics m: Metrics, context: CGContext) -> Bool {
    let scalars = Array(text.unicodeScalars)
    guard scalars.count == 1 else { return false }
    let value = scalars[0].value
    if let block = blockRect(value, in: rect) {
        context.setFillColor(color)
        context.fill(block)
        return true
    }
    guard let edges = boxEdges[value] else { return false }
    let t = m.lineWidth
    // The left edge of the vertical line and the bottom edge of the horizontal one, on whole
    // pixels so lines stay sharp and meet the next cell's.
    let vx = (rect.midX - t / 2).rounded(.down)
    let hy = (rect.midY - t / 2).rounded(.down)
    if edges.contains("~") {
        let center = CGPoint(x: vx + t / 2, y: hy + t / 2)
        let horizontalEnd = edges.contains("l") ? rect.minX : rect.maxX
        let verticalEnd = edges.contains("u") ? rect.maxY : rect.minY
        let radius = min(abs(horizontalEnd - center.x), abs(verticalEnd - center.y))
        let path = CGMutablePath()
        path.move(to: CGPoint(x: horizontalEnd, y: center.y))
        path.addArc(tangent1End: center, tangent2End: CGPoint(x: center.x, y: verticalEnd), radius: radius)
        path.addLine(to: CGPoint(x: center.x, y: verticalEnd))
        context.setStrokeColor(color)
        context.setLineWidth(t)
        context.setLineCap(.butt)
        context.addPath(path)
        context.strokePath()
        return true
    }
    context.setFillColor(color)
    if edges.contains("l") {
        context.fill(CGRect(x: rect.minX, y: hy, width: vx + t - rect.minX, height: t))
    }
    if edges.contains("r") {
        context.fill(CGRect(x: vx, y: hy, width: rect.maxX - vx, height: t))
    }
    if edges.contains("u") {
        context.fill(CGRect(x: vx, y: hy, width: t, height: rect.maxY - hy))
    }
    if edges.contains("d") {
        context.fill(CGRect(x: vx, y: rect.minY, width: t, height: hy + t - rect.minY))
    }
    return true
}

func drawText(_ text: String, font: CTFont, color: CGColor, in rect: CGRect, baseline: CGFloat, context: CGContext) {
    let attributes: [NSAttributedString.Key: Any] = [
        NSAttributedString.Key(kCTFontAttributeName as String): font,
        NSAttributedString.Key(kCTForegroundColorAttributeName as String): color,
    ]
    let string = NSAttributedString(string: text, attributes: attributes)
    let line = CTLineCreateWithAttributedString(string as CFAttributedString)
    let width = CGFloat(CTLineGetTypographicBounds(line, nil, nil, nil))
    guard width > 0 else { return }
    // A fallback glyph, such as an emoji, can be wider than its cells. Shrink it to fit.
    let fit = width > rect.width + 0.5 ? rect.width / width : 1
    context.saveGState()
    context.translateBy(x: rect.minX + (rect.width - width * fit) / 2, y: rect.minY + baseline)
    context.scaleBy(x: fit, y: fit)
    context.textPosition = .zero
    CTLineDraw(line, context)
    context.restoreGState()
}

func render(_ screen: [[Cell]], options: Options, fonts: Fonts, metrics m: Metrics) -> CGImage {
    let cols = options.cols
    let pad = (options.padding * options.scale).rounded()
    let width = Int(CGFloat(cols) * m.cellWidth + 2 * pad)
    let height = Int(CGFloat(screen.count) * m.cellHeight + 2 * pad)
    guard let context = CGContext(
        data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0, space: sRGB,
        bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue
    ) else {
        fail("can't make a \(width)x\(height) bitmap")
    }

    // CoreGraphics puts y = 0 at the bottom, so row 0 is the top band of the image.
    func cellRect(_ row: Int, _ col: Int, span: Int = 1) -> CGRect {
        CGRect(
            x: pad + CGFloat(col) * m.cellWidth,
            y: CGFloat(height) - pad - CGFloat(row + 1) * m.cellHeight,
            width: CGFloat(span) * m.cellWidth,
            height: m.cellHeight
        )
    }

    context.setShouldAntialias(false)
    context.setFillColor(cgColor(options.defaultBG))
    context.fill(CGRect(x: 0, y: 0, width: width, height: height))
    for (row, cells) in screen.enumerated() {
        for (col, cell) in cells.enumerated() {
            context.setFillColor(cgColor(cellColors(cell.style, options).bg))
            context.fill(cellRect(row, col))
        }
    }

    context.setShouldAntialias(true)
    for (row, cells) in screen.enumerated() {
        for (col, cell) in cells.enumerated() where cell.width > 0 {
            let rect = cellRect(row, col, span: min(cell.width, cols - col))
            let fg = cgColor(cellColors(cell.style, options).fg)
            if cell.style.hidden { continue }
            if cell.text != " " {
                let drawn = !options.fontBoxes && drawShape(cell.text, in: rect, color: fg, metrics: m, context: context)
                if !drawn {
                    let font = fonts.font(bold: cell.style.bold, italic: cell.style.italic)
                    drawText(cell.text, font: font, color: fg, in: rect, baseline: m.baseline, context: context)
                }
            }
            context.setFillColor(fg)
            if cell.style.underline {
                let y = max(rect.minY, rect.minY + m.baseline - m.underlineDepth)
                context.fill(CGRect(x: rect.minX, y: y, width: rect.width, height: m.lineWidth))
            }
            if cell.style.strike {
                let y = rect.minY + m.baseline + m.strikeHeight
                context.fill(CGRect(x: rect.minX, y: y, width: rect.width, height: m.lineWidth))
            }
        }
    }

    guard let image = context.makeImage() else { fail("can't finish the image") }
    return image
}

// MARK: - Main

let options = parseOptions()

let inputURL = URL(fileURLWithPath: options.input)
let outputURL = URL(fileURLWithPath: options.output)
guard outputURL.pathExtension.lowercased() == "png" else { fail("OUT must end in .png") }
let inputFolder = inputURL.deletingLastPathComponent().resolvingSymlinksInPath().path
let outputFolder = outputURL.deletingLastPathComponent().resolvingSymlinksInPath().path
guard inputFolder != outputFolder else {
    fail("write OUT outside \(inputFolder) so the source captures stay as they are")
}
guard let data = FileManager.default.contents(atPath: options.input) else {
    fail("can't read \(options.input)")
}

let parser = Parser(cols: options.cols)
parser.read(String(decoding: data, as: UTF8.self))
var screen = parser.rows
let rowCount = options.rows ?? screen.count
guard screen.count <= rowCount else {
    fail("\(options.input) has \(screen.count) lines but --rows is \(rowCount)")
}
if !parser.overflow.isEmpty {
    for item in parser.overflow {
        warn("row \(item.row + 1) takes \(item.width) columns but --cols is \(options.cols)")
    }
    guard options.allowOverflow else {
        fail("these widths don't match the capture, so no image was written", code: 2)
    }
}
if parser.skippedSequences > 0 || parser.skippedCharacters > 0 {
    warn("skipped \(parser.skippedSequences) escape sequences other than SGR and \(parser.skippedCharacters) characters that aren't cell content")
}
let blankRow = Array(repeating: Cell(text: " ", width: 1, style: Style()), count: options.cols)
let capturedRows = screen.count
while screen.count < rowCount {
    screen.append(blankRow)
}

let fonts = Fonts(name: options.font, size: options.fontSize * options.scale)
let metrics = Metrics(font: fonts.regular, options: options)
let image = render(screen, options: options, fonts: fonts, metrics: metrics)

guard let destination = CGImageDestinationCreateWithURL(outputURL as CFURL, "public.png" as CFString, 1, nil) else {
    fail("can't write \(options.output)")
}
let dpi = 72 * options.scale
CGImageDestinationAddImage(destination, image, [kCGImagePropertyDPIWidth: dpi, kCGImagePropertyDPIHeight: dpi] as CFDictionary)
guard CGImageDestinationFinalize(destination) else { fail("can't write \(options.output)") }

let padded = rowCount > capturedRows ? ", \(rowCount - capturedRows) blank rows added" : ""
print("\(options.input) -> \(options.output): \(options.cols)x\(rowCount) cells\(padded), \(image.width)x\(image.height) px, \(options.font) \(options.fontSize)pt at \(options.scale)x, cell \(Int(metrics.cellWidth))x\(Int(metrics.cellHeight)) px")
