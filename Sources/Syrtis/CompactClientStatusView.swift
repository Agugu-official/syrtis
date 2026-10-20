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

        /// The widest text the item can show, so its width does not follow
        /// each countdown tick and neighbouring menu-bar items stay put.
        private static let reservedTextWidth: CGFloat = {
            let countdowns = [
                UsagePace.durationText(29 * 86_400 + 23 * 3_600),
                UsagePace.durationText(23 * 3_600 + 59 * 60),
                "Resetting".localized,
            ]
            let widths = [line("100%", percent: true, color: .labelColor)]
                + countdowns.map { line($0, percent: false, color: .labelColor) }
            return ceil(widths.map {
                CTLineGetBoundsWithOptions($0, .useGlyphPathBounds).width
            }.max() ?? 0)
        }()

        var textWidth: CGFloat { ceil(max(percentInk.width, countdownInk.width)) }
        var textHeight: CGFloat { percentInk.height + rowGap + countdownInk.height }
        var width: CGFloat {
            sidePadding * 2 + iconSize + iconGap + max(textWidth, Self.reservedTextWidth)
        }

        func frames(in bounds: CGRect) -> (icon: CGRect, percent: CGRect, countdown: CGRect) {
            let left = bounds.midX - (iconSize + iconGap + textWidth) / 2
            let textLeft = left + iconSize + iconGap
            let bottom = bounds.midY - textHeight / 2
            return (
                CGRect(x: left, y: bounds.midY - iconSize / 2, width: iconSize, height: iconSize),
                CGRect(
                    x: textLeft + (textWidth - percentInk.width) / 2,
                    y: bottom + countdownInk.height + rowGap,
                    width: percentInk.width, height: percentInk.height),
                CGRect(
                    x: textLeft + (textWidth - countdownInk.width) / 2, y: bottom,
                    width: countdownInk.width, height: countdownInk.height))
        }

        func draw(in bounds: CGRect, icon: NSImage) {
            let rects = frames(in: bounds)
            icon.draw(in: rects.icon, from: .zero, operation: .sourceOver, fraction: 1)
            guard let context = NSGraphicsContext.current?.cgContext else { return }
            context.saveGState()
            context.textMatrix = .identity
            // Position the visible glyph bounds, not the font's ascender box.
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
