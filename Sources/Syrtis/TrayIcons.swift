import AppKit

/// Shared quota thresholds for automatic gauges and custom menu-bar text.
enum QuotaColorLevel: String, CaseIterable, Identifiable {
    case normal, warning, critical

    var id: String { rawValue }

    init(remaining: Double) {
        if remaining <= 10 { self = .critical }
        else if remaining <= 25 { self = .warning }
        else { self = .normal }
    }
}

/// Quota-gauge icon styles for the status item (the cat/parrot frame sets
/// live in TrayAnimator's resources; these are drawn programmatically).
enum QuotaIconStyle: String, CaseIterable {
    case bars, ring, popsicle

    var label: String {
        switch self {
        case .bars: return "Signal bars"
        case .ring: return "Ring gauge"
        case .popsicle: return "Melting popsicle"
        }
    }
}

/// When the quota icons pick up the gauge color. Mirrors the battery icon's
/// behavior in `warningOnly` (color only when it matters).
enum IconColoring: String, CaseIterable {
    case warningOnly = "warning"
    case always
    case never

    static let storageKey = "tokenbar.icon.coloring"

    var label: String {
        switch self {
        case .warningOnly: return "Color on warning only"
        case .always: return "Always colored"
        case .never: return "Never colored"
        }
    }
}

/// Programmatic menu-bar gauge icons. All drawing is on a 16x16 design grid
/// via resolution-independent drawing handlers (crisp on retina).
enum TrayIcons {
    /// limits-card gauge palette: green / amber under 25% left / red under 10%.
    static func gaugeColor(remaining: Double) -> NSColor {
        switch QuotaColorLevel(remaining: remaining) {
        case .critical: NSColor(srgbRed: 0.94, green: 0.27, blue: 0.27, alpha: 1)
        case .warning: NSColor(srgbRed: 0.96, green: 0.62, blue: 0.04, alpha: 1)
        case .normal: NSColor(srgbRed: 0.13, green: 0.77, blue: 0.37, alpha: 1)
        }
    }

    /// The fill ink for a gauge icon under the chosen coloring policy.
    /// The grey a stale reading is drawn in: the gauge fill, and since #420
    /// the quota title beside it, so the two read as one state.
    static func staleInk(dark: Bool) -> NSColor {
        NSColor(white: dark ? staleFillWhiteDark : staleFillWhiteLight, alpha: 1)
    }

    static func ink(remaining: Double?, dark: Bool, coloring: IconColoring) -> NSColor {
        let mono: NSColor = dark ? .white : .black
        guard let remaining else { return mono }
        switch coloring {
        case .never: return mono
        case .always: return gaugeColor(remaining: remaining)
        case .warningOnly: return QuotaColorLevel(remaining: remaining) != .normal ? gaugeColor(remaining: remaining) : mono
        }
    }

    /// Fill for a stale reading (#8): the look of a half-transparent mono ink,
    /// drawn opaque. A real 0.5 alpha is not usable because the popsicle's fill
    /// shapes overlap, and each overlap doubles the alpha into a visible bright
    /// stripe (seen in a 2026-09-27 render). Grey levels sit between the 0.3
    /// track and full mono on the menu bar's dark and light backgrounds.
    /// Tune here only.
    static let staleFillWhiteDark = 0.70
    static let staleFillWhiteLight = 0.40

    /// Width added in front of the 16-point gauge for the window letter.
    static let prefixWidth: CGFloat = 9

    /// Draws the letter's glyph directly instead of typesetting a string.
    /// v2.9.0 drew an NSAttributedString here, and CoreText intermittently
    /// threw "attempt to insert nil object" from TAttributes::ApplyFont while
    /// sizing it inside this drawing handler (seen on a status-item replicant
    /// redraw after 3 h 52 min on a test build, and once within 20 s in a tight
    /// loop of the same drawing, which a dozen later minute-long loops did not
    /// repeat), aborting the app. CTFontDrawGlyphs draws glyphs without
    /// building an attributed line, so that frame is never reached. Placement reproduces the old
    /// `size()`/`draw(at:)` pixel for pixel (compared for H, D, W and M at 2x):
    /// advance width centred in the lead, the line height rounded up as
    /// `size()` does and centred, baseline the rounded-up descent above it.
    static func drawLetter(_ letter: String, color: NSColor, lead: CGFloat) {
        // Monospaced: H, D, W and M are one width, so the letter always fits
        // prefixWidth and the icon does not change width between them (the
        // proportional W overflowed the 9 pt lead on the left).
        let font: CTFont = NSFont.monospacedSystemFont(ofSize: 10.5, weight: .semibold)
        var chars = Array(letter.utf16)
        var glyphs = [CGGlyph](repeating: 0, count: chars.count)
        guard !chars.isEmpty, CTFontGetGlyphsForCharacters(font, &chars, &glyphs, chars.count),
              let cg = NSGraphicsContext.current?.cgContext else { return }
        let advance = CTFontGetAdvancesForGlyphs(font, .horizontal, glyphs, nil, 1)
        let descent = CTFontGetDescent(font)
        let height = (CTFontGetAscent(font) + descent + CTFontGetLeading(font)).rounded(.up)
        var origin = CGPoint(x: (lead - 1 - advance) / 2, y: (16 - height) / 2 + descent.rounded(.up))
        cg.saveGState()
        cg.setFillColor(color.cgColor)
        CTFontDrawGlyphs(font, glyphs, &origin, 1, cg)
        cg.restoreGState()
    }

    static func image(
        style: QuotaIconStyle, remaining: Double?, dark: Bool, coloring: IconColoring,
        stale: Bool = false, prefix: String? = nil
    ) -> NSImage {
        let mono: NSColor = dark ? .white : .black
        // A stale reading drops the coloring policy: a green or red that is an
        // hour old would still read as a live verdict. With no reading there is
        // nothing to be stale, so the no-reading glyph (`drawNoReading`) is
        // kept whatever the stamp says.
        let fill = stale && remaining != nil
            ? staleInk(dark: dark)
            : ink(remaining: remaining, dark: dark, coloring: coloring)
        let level = remaining ?? 100
        // No letter without a reading: it would name a window nothing was read from.
        let letter = remaining == nil ? nil : prefix
        let lead: CGFloat = letter == nil ? 0 : prefixWidth
        let image = NSImage(size: NSSize(width: 16 + lead, height: 16), flipped: false) { _ in
            if let letter {
                drawLetter(letter, color: stale ? staleInk(dark: dark) : mono, lead: lead)
                NSGraphicsContext.current?.cgContext.translateBy(x: lead, y: 0)
            }
            guard remaining != nil else {
                drawNoReading(style: style, mono: mono)
                return true
            }
            switch style {
            case .bars: drawBars(remaining: level, mono: mono, fill: fill)
            case .ring: drawRing(remaining: level, mono: mono, fill: fill)
            case .popsicle: drawPopsicle(remaining: level, mono: mono, fill: fill)
            }
            return true
        }
        image.isTemplate = false
        return image
    }

    // MARK: - No reading (#419)

    /// With no reading the gauge used to be drawn at 100% in mono, which is
    /// what a full allowance looks like. It is now the full shape, faded, with
    /// a "no signal" slash, so it reads as neither full nor empty (0% keeps
    /// its bright empty track). Chosen by the maintainer from renders of every
    /// style in dark and light.
    ///
    /// The shape is drawn opaque inside a transparency layer and faded as a
    /// whole, so the popsicle's overlapping stick and body do not show through
    /// each other.
    static let noReadingAlpha: CGFloat = 0.3
    /// Slash stroke, and the wider gap cut out of the faded shape around it.
    static let noReadingSlashWidth: CGFloat = 1.5
    static let noReadingSlashGap: CGFloat = 3.4

    private static func drawNoReading(style: QuotaIconStyle, mono: NSColor) {
        guard let cg = NSGraphicsContext.current?.cgContext else { return }
        cg.saveGState()
        cg.setAlpha(noReadingAlpha)
        cg.beginTransparencyLayer(auxiliaryInfo: nil)
        switch style {
        case .bars: drawBars(remaining: 100, mono: mono, fill: mono)
        case .ring: drawRing(remaining: 100, mono: mono, fill: mono)
        case .popsicle: drawPopsicle(remaining: 100, mono: mono, fill: mono)
        }
        cg.endTransparencyLayer()
        cg.restoreGState()

        let slash = NSBezierPath()
        slash.move(to: point(2.5, 13.5))
        slash.line(to: point(13.5, 2.5))
        slash.lineCapStyle = .round
        NSGraphicsContext.current?.compositingOperation = .destinationOut
        slash.lineWidth = noReadingSlashGap
        NSColor.black.setStroke()
        slash.stroke()
        NSGraphicsContext.current?.compositingOperation = .sourceOver
        slash.lineWidth = noReadingSlashWidth
        mono.withAlphaComponent(0.9).setStroke()
        slash.stroke()
    }

    // MARK: - Shared 16-grid helpers

    private static func rect(_ x: Double, _ y: Double, _ w: Double, _ h: Double) -> NSRect {
        NSRect(x: x, y: y, width: w, height: h)
    }

    private static func point(_ x: Double, _ y: Double) -> NSPoint { NSPoint(x: x, y: y) }

    // MARK: - Staircase bars with a continuous waterline

    /// Four ascending bars as the outline; the remaining percent sweeps a
    /// continuous fill from the left (volume-HUD metaphor).
    private static func drawBars(remaining: Double, mono: NSColor, fill: NSColor) {
        let heights: [Double] = [5, 8, 11, 14]
        let fillX = 16.0 * min(100, max(0, remaining)) / 100
        for (i, h) in heights.enumerated() {
            let x = Double(i) * 4.0
            let bar = rect(x, 1, 3, h)
            let path = NSBezierPath(roundedRect: bar, xRadius: 1.2, yRadius: 1.2)
            mono.withAlphaComponent(0.3).setFill()
            path.fill()
            let visible = max(0, min(3, fillX - x))
            guard visible > 0 else { continue }
            NSGraphicsContext.current?.saveGraphicsState()
            NSBezierPath(rect: rect(x, 1, visible, h)).addClip()
            fill.setFill()
            path.fill()
            NSGraphicsContext.current?.restoreGraphicsState()
        }
    }

    // MARK: - Ring gauge

    private static func drawRing(remaining: Double, mono: NSColor, fill: NSColor) {
        let center = point(8, 8)
        let radius = 5.6
        let track = NSBezierPath()
        track.appendArc(withCenter: center, radius: radius, startAngle: 0, endAngle: 360)
        track.lineWidth = 2.6
        mono.withAlphaComponent(0.28).setStroke()
        track.stroke()
        let sweep = 360.0 * min(100, max(0, remaining)) / 100
        guard sweep > 1 else { return }
        let arc = NSBezierPath()
        arc.appendArc(
            withCenter: center, radius: radius,
            startAngle: 90, endAngle: 90 - sweep, clockwise: true)
        arc.lineWidth = 2.6
        arc.lineCapStyle = .round
        fill.setStroke()
        arc.stroke()
    }

    // MARK: - Melting popsicle (v7: square shoulders, grooves, hugging dome)

    /// The remaining mass shrinks concentrically toward the stick collar; the
    /// stick tip pokes out as it melts, drips fall and a puddle grows. Two
    /// classic face grooves melt away first. Depth via opacity layers only.
    private static func drawPopsicle(remaining: Double, mono: NSColor, fill ice: NSColor) {
        let r = min(100, max(0, remaining)) / 100
        let melt = 1 - r

        // stick (same ink, lighter layer)
        mono.withAlphaComponent(0.55).setFill()
        NSBezierPath(roundedRect: rect(7.25, 1.5, 1.5, 10.5), xRadius: 0.75, yRadius: 0.75).fill()

        // runoff puddle
        if melt > 0.3 {
            let pw = 3 + 9 * (melt - 0.3) / 0.7
            ice.withAlphaComponent(0.35).setFill()
            NSBezierPath(ovalIn: rect(8 - pw / 2, 0.5, pw, 1.4)).fill()
        }

        guard remaining > 3 else { return }
        let topY = 5.0 + 10.0 * r
        let w = 2.6 + 5.4 * r
        let halfW = w / 2
        let capR = min(1.8, (topY - 5.0) * 0.32)
        let dip = 0.5 + 0.9 * melt
        let shoulder = 0.12 * melt

        let body = NSBezierPath()
        body.move(to: point(8 - halfW, 5.6))
        body.curve(
            to: point(8 + halfW, 5.6),
            controlPoint1: point(8 - halfW * 0.5, 5.6 - dip),
            controlPoint2: point(8 + halfW * 0.5, 5.6 - dip))
        body.curve(
            to: point(8 + halfW - shoulder - capR * 0.3, topY - capR * 0.2),
            controlPoint1: point(8 + halfW + 0.15, 5.6 + (topY - 5.6) * 0.5),
            controlPoint2: point(8 + halfW - shoulder + 0.1, topY - capR))
        body.curve(
            to: point(8 - halfW + shoulder + capR * 0.3, topY - capR * 0.2),
            controlPoint1: point(8 + halfW * 0.45, topY + capR * 0.3 - 0.25 * melt),
            controlPoint2: point(8 - halfW * 0.45, topY + capR * 0.3 - 0.25 * melt))
        body.curve(
            to: point(8 - halfW, 5.6),
            controlPoint1: point(8 - halfW + shoulder - 0.1, topY - capR),
            controlPoint2: point(8 - halfW - 0.15, 5.6 + (topY - 5.6) * 0.5))
        body.close()
        ice.setFill()
        body.fill()

        // two classic face grooves, punched out; gone below ~45%
        if remaining > 45 {
            let presence = min(1, (r - 0.45) / 0.4)
            let inset = w / 3.2
            let gBottom = 6.6
            let gTop = topY - capR - 0.9
            if gTop > gBottom + 0.8 {
                NSGraphicsContext.current?.compositingOperation = .destinationOut
                for gx in [8 - inset, 8 + inset] {
                    NSColor.black.withAlphaComponent(0.85 * presence).setFill()
                    NSBezierPath(
                        roundedRect: rect(gx - 0.33, gBottom, 0.66, gTop - gBottom),
                        xRadius: 0.33, yRadius: 0.33
                    ).fill()
                }
                NSGraphicsContext.current?.compositingOperation = .sourceOver
            }
        }

        // hanging drip + falling droplet
        if melt > 0.15 {
            ice.withAlphaComponent(0.7).setFill()
            NSBezierPath(ovalIn: rect(8.5, 5.6 - dip - 1.5, 1.0, 1.5)).fill()
        }
        if melt > 0.5 {
            ice.withAlphaComponent(0.45).setFill()
            NSBezierPath(ovalIn: rect(6.7, 2.8, 0.85, 1.1)).fill()
        }
    }
}
