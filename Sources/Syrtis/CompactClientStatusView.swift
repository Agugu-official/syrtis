import AppKit
import CoreText
import TokenBarCore

/// Draws inside the native status button; the button still owns input,
/// highlighting, the tooltip, and accessibility.
@MainActor
final class CompactClientStatusView: NSView {
    struct Layout {
        let percentLine: CTLine
        let countdownLine: CTLine
        let percentInk: CGRect
        let countdownInk: CGRect
        let iconSize: CGFloat = 18
        let iconGap: CGFloat = 6
        let rowGap: CGFloat = 3
        let sidePadding: CGFloat = 6

        init(percent: String, countdown: String, color: NSColor) {
            percentLine = Self.line(percent, percent: true, color: color)
            countdownLine = Self.line(countdown, percent: false, color: color)
            percentInk = CTLineGetBoundsWithOptions(percentLine, .useGlyphPathBounds)
            countdownInk = CTLineGetBoundsWithOptions(countdownLine, .useGlyphPathBounds)
        }

        private static func line(_ text: String, percent: Bool, color: NSColor) -> CTLine {
            CTLineCreateWithAttributedString(NSAttributedString(
                string: text, attributes: [
                    .font: NSFont.monospacedDigitSystemFont(
                        ofSize: percent ? 10 : 9, weight: percent ? .semibold : .regular),
                    .foregroundColor: color,
                ]))
        }

        private static let referencePercents = ["100%", "—%"]
        private static let referenceCountdowns = [
            UsagePace.durationText(29 * 86_400 + 23 * 3_600),
            UsagePace.durationText(23 * 3_600 + 59 * 60),
            "Resetting".localized,
            "—",
        ]

        /// The widest text the item can show, so its width does not follow
        /// each countdown tick and neighbouring menu-bar items stay put.
        private static let reservedTextWidth: CGFloat = {
            let widths = referencePercents.map { line($0, percent: true, color: .labelColor) }
                + referenceCountdowns.map { line($0, percent: false, color: .labelColor) }
            return ceil(widths.map {
                CTLineGetBoundsWithOptions($0, .useGlyphPathBounds).width
            }.max() ?? 0)
        }()

        /// The vertical counterpart: the ink every percent or countdown can
        /// reach around its baseline. Laying out these bands instead of the
        /// current glyphs keeps both baselines fixed, so the em dash shown
        /// without a countdown (almost no height) or the descender in
        /// "Resetting" does not move the percent line.
        private static let reservedPercentInk = inkUnion(referencePercents, percent: true)
        private static let reservedCountdownInk = inkUnion(referenceCountdowns, percent: false)

        private static func inkUnion(_ texts: [String], percent: Bool) -> CGRect {
            texts.map {
                CTLineGetBoundsWithOptions(
                    line($0, percent: percent, color: .labelColor), .useGlyphPathBounds)
            }.reduce(CGRect.null) { $0.union($1) }
        }

        var textWidth: CGFloat { ceil(max(percentInk.width, countdownInk.width)) }
        var textHeight: CGFloat {
            Self.reservedPercentInk.height + rowGap + Self.reservedCountdownInk.height
        }
        var width: CGFloat {
            sidePadding * 2 + iconSize + iconGap + max(textWidth, Self.reservedTextWidth)
        }

        /// `text` is the reserved two-line box, centered with the icon; the
        /// glyph rects sit on its fixed baselines.
        func frames(in bounds: CGRect)
            -> (icon: CGRect, text: CGRect, percent: CGRect, countdown: CGRect)
        {
            // Centered on the reserved width so the icon stays put as the
            // countdown changes; the text sits at the left of that box.
            let reservedWidth = max(textWidth, Self.reservedTextWidth)
            let left = bounds.midX - (iconSize + iconGap + reservedWidth) / 2
            let textLeft = left + iconSize + iconGap
            let bottom = bounds.midY - textHeight / 2
            let countdownBaseline = bottom - Self.reservedCountdownInk.minY
            let percentBaseline = bottom + Self.reservedCountdownInk.height + rowGap
                - Self.reservedPercentInk.minY
            return (
                CGRect(x: left, y: bounds.midY - iconSize / 2, width: iconSize, height: iconSize),
                CGRect(x: textLeft, y: bottom, width: reservedWidth, height: textHeight),
                CGRect(
                    x: textLeft + (textWidth - percentInk.width) / 2,
                    y: percentBaseline + percentInk.minY,
                    width: percentInk.width, height: percentInk.height),
                CGRect(
                    x: textLeft + (textWidth - countdownInk.width) / 2,
                    y: countdownBaseline + countdownInk.minY,
                    width: countdownInk.width, height: countdownInk.height))
        }

        func draw(in bounds: CGRect, icon: NSImage) {
            let rects = frames(in: bounds)
            icon.draw(in: rects.icon, from: .zero, operation: .sourceOver, fraction: 1)
            guard let context = NSGraphicsContext.current?.cgContext else { return }
            context.saveGState()
            context.textMatrix = .identity
            // The glyph rects carry the fixed baselines; draw from them.
            context.textPosition = CGPoint(
                x: rects.percent.minX - percentInk.minX,
                y: rects.percent.minY - percentInk.minY)
            CTLineDraw(percentLine, context)
            context.textPosition = CGPoint(
                x: rects.countdown.minX - countdownInk.minX,
                y: rects.countdown.minY - countdownInk.minY)
            CTLineDraw(countdownLine, context)
            context.restoreGState()
        }
    }

    let icon: NSImage
    var percent = ""
    var countdown = ""
    var textColor: NSColor?

    init(icon: NSImage) {
        self.icon = icon
        super.init(frame: .zero)
        autoresizingMask = [.width, .height]
        setAccessibilityElement(false)
    }

    required init?(coder: NSCoder) { nil }

    var contentLayout: Layout {
        Layout(percent: percent, countdown: countdown, color: textColor ?? .labelColor)
    }

    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsDisplay = true
    }

    override func draw(_ dirtyRect: NSRect) {
        contentLayout.draw(in: bounds, icon: icon)
    }
}
