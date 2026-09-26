# macOS overlay — what it needs

Not built yet. This is the shape of it, so the next session starts from a
decision rather than a blank file.

## The window

A borderless, transparent `NSWindow` that floats over everything and never
steals focus or clicks.

```swift
let window = NSWindow(
    contentRect: NSScreen.main!.frame,
    styleMask: [.borderless],
    backing: .buffered,
    defer: false)

window.isOpaque = false
window.backgroundColor = .clear
window.hasShadow = false
window.ignoresMouseEvents = true          // clicks pass through to your apps
window.level = .screenSaver               // above normal windows and full-screen
window.collectionBehavior = [.canJoinAllSpaces, .stationary, .fullScreenAuxiliary]
```

`ignoresMouseEvents = true` is the whole trick: the fly is drawn on top of your
work but cannot interfere with it. Cursor position still comes through — use a
global monitor rather than the window's own events:

```swift
NSEvent.addGlobalMonitorForEvents(matching: [.mouseMoved]) { event in
    // NSEvent.mouseLocation is in screen coordinates, origin bottom-left
}
```

## Vision

`ScreenCaptureKit` (macOS 12.3+), not the deprecated `CGWindowListCreateImage`.
Capture a window around the fly rather than the whole screen — a 256×256 patch
at 30 Hz is a fraction of the cost of a full display and is all the fly's eye
resolution can use anyway.

```swift
let config = SCStreamConfiguration()
config.width = 256
config.height = 256
config.minimumFrameInterval = CMTime(value: 1, timescale: 30)
config.sourceRect = CGRect(x: flyX - 128, y: flyY - 128, width: 256, height: 256)
config.showsCursor = false                 // or the fly sees itself being chased
```

This prompts for Screen Recording permission on first run. There is no way
around that and no point trying; explain it in the UI instead.

Exclude the overlay window itself from the capture via
`SCContentFilter(display:excludingWindows:)`, or the fly ends up looking at a
picture of itself.

## Talking to the brain

`URLSessionWebSocketTask` to `ws://localhost:8787`. Send what the fly can see,
read back what its descending neurons say.

Out, at capture rate:

```json
{"threat": {"bearing": 0.42, "size": 12.5}}
{"turn": 1.83}
```

`bearing` is fly-centric radians, 0 straight ahead. `size` is angular size in
the same arbitrary units the service was tuned with — what matters is that it
grows as things approach, because the looming detectors respond to the rate of
growth far more than the magnitude. `turn` closes the loop: the body telling
the brain it rotated, which is what updates the heading bump.

In, at 60 Hz:

```json
{"t":128400,"turn":-18.2,"forward":176,"escapeRun":0,"escapeJump":0,
 "stop":0,"heading":1.42,"headingConfidence":0.73,"spikeRate":4820}
```

Rates are Hz. The mapping to screen motion is in `clients/web/index.html`
(`updateBody`) and is deliberately thin — a scale factor and a first-order lag
standing in for body inertia. Port it rather than reinventing it, so the two
clients behave the same.

## Drawing

`CAMetalLayer` if you want the fly to cost nothing, but a `CALayer` with a
Core Graphics redraw at 60 Hz is fine at this size and much less work. The
procedural fly in the web client (body, wings, six legs on a tripod gait) ports
to Core Graphics almost line for line.

Alternative worth considering: skip Swift, run the existing web client in a
transparent `WKWebView` inside the overlay window. You get the renderer for
free and lose maybe a millisecond a frame. For a first version that is a good
trade.

## Ordering

1. Overlay window with a static fly, click-through verified.
2. WebSocket to the brain, fly walks and reacts to cursor position only.
3. ScreenCaptureKit feeding a real optic lobe.

Step 3 is the interesting one and also the one that needs the optic lobe built
first, so do not start there.

## Packaging

Needs `NSScreenCaptureUsageDescription` in `Info.plist`. Ship the brain service
as a bundled JAR launched by the app, or rewrite the core in Swift once it
stops changing — the LIF loop is about 80 lines and the pack loader another 60.
Keeping Java means one brain implementation instead of three.
