// oxbot-mac: the Mac's own media work for oxbot. It never uses the network:
// still frames come from AVFoundation, and dictation is Apple's on-device
// speech recognition only (`requiresOnDeviceRecognition`). Audio is never
// written to disk; only text leaves this process.
//
//   oxbot-mac version
//   oxbot-mac can-dictate [LOCALE]
//       {"ok":true,"locale":"en-US"} or {"ok":false,"why":"…"}
//   oxbot-mac dictate [LOCALE] [--file AUDIO]
//       lines of {"text":"…","final":false}, then {"text":"…","final":true}.
//       From the mic, it stops when stdin closes or after 120 s. --file
//       reads an audio file instead of the mic (for tests).
//   oxbot-mac frames VIDEO OUTDIR [--max 8] [--size 1024]
//       writes OUTDIR/01.jpg … and prints {"duration_ms":N,"frames":N}
//
// Errors print {"error":"…"} and exit 1.

import AVFoundation
import Foundation
import ImageIO
import Speech

let version = "1"

func emit(_ object: [String: Any]) {
    guard let data = try? JSONSerialization.data(withJSONObject: object),
          var line = String(data: data, encoding: .utf8)
    else { return }
    line += "\n"
    FileHandle.standardOutput.write(Data(line.utf8))
}

func fail(_ why: String) -> Never {
    emit(["error": why])
    exit(1)
}

/// The value after `--name`, if given.
func option(_ args: [String], _ name: String) -> String? {
    guard let i = args.firstIndex(of: name), i + 1 < args.count else { return nil }
    return args[i + 1]
}

/// The arguments that aren't options or their values.
func plain(_ args: [String]) -> [String] {
    var out: [String] = []
    var skip = false
    for a in args {
        if skip { skip = false; continue }
        if a.hasPrefix("--") { skip = true; continue }
        out.append(a)
    }
    return out
}

func recognizer(_ locale: String?) -> SFSpeechRecognizer? {
    if let locale = locale, !locale.isEmpty {
        return SFSpeechRecognizer(locale: Locale(identifier: locale))
    }
    return SFSpeechRecognizer()
}

// MARK: dictation

func canDictate(_ locale: String?) {
    guard let r = recognizer(locale) else {
        emit(["ok": false, "why": "This Mac has no speech recognition for this language."])
        return
    }
    guard r.supportsOnDeviceRecognition else {
        emit(["ok": false, "why": "On-device dictation isn't available for \(r.locale.identifier). Turn on Dictation in System Settings → Keyboard."])
        return
    }
    switch SFSpeechRecognizer.authorizationStatus() {
    case .denied, .restricted:
        emit(["ok": false, "why": "Speech recognition is turned off for oxbot in System Settings → Privacy & Security."])
    default:
        emit(["ok": true, "locale": r.locale.identifier])
    }
}

/// Asks macOS for speech recognition, then the mic (unless reading a
/// file), and calls `then` on the main queue if both are allowed. The
/// first time, macOS asks the person; nobody answering in 60 s is a no.
func authorize(mic: Bool, then: @escaping () -> Void) {
    DispatchQueue.main.asyncAfter(deadline: .now() + 60) {
        fail("macOS didn't get an answer about speech recognition. Try the microphone again.")
    }
    SFSpeechRecognizer.requestAuthorization { status in
        DispatchQueue.main.async {
            guard status == .authorized else {
                fail("Speech recognition is turned off for oxbot in System Settings → Privacy & Security.")
            }
            guard mic else { return then() }
            AVCaptureDevice.requestAccess(for: .audio) { granted in
                DispatchQueue.main.async {
                    guard granted else {
                        fail("The microphone is turned off for oxbot in System Settings → Privacy & Security.")
                    }
                    then()
                }
            }
        }
    }
}

var lastText = ""
var finished = false

/// Prints the last text as final, once, and exits.
func finish() -> Never {
    if !finished {
        finished = true
        emit(["text": lastText, "final": true])
    }
    exit(0)
}

/// Prints each new piece of text; the final one ends the program.
func handle(_ result: SFSpeechRecognitionResult?, _ error: Error?) {
    if let result = result {
        lastText = result.bestTranscription.formattedString
        if result.isFinal { finish() }
        emit(["text": lastText, "final": false])
    } else if error != nil {
        // "No speech detected" and a stop both end here.
        finish()
    }
}

func dictateFile(_ r: SFSpeechRecognizer, _ file: String) {
    let request = SFSpeechURLRecognitionRequest(url: URL(fileURLWithPath: file))
    request.requiresOnDeviceRecognition = true
    request.shouldReportPartialResults = true
    _ = r.recognitionTask(with: request, resultHandler: handle)
    DispatchQueue.main.asyncAfter(deadline: .now() + 60) { finish() }
}

func dictateMic(_ r: SFSpeechRecognizer) {
    let request = SFSpeechAudioBufferRecognitionRequest()
    request.requiresOnDeviceRecognition = true
    request.shouldReportPartialResults = true
    let engine = AVAudioEngine()
    let input = engine.inputNode
    let format = input.outputFormat(forBus: 0)
    guard format.sampleRate > 0 else { fail("This Mac has no microphone.") }
    input.installTap(onBus: 0, bufferSize: 1024, format: format) { buffer, _ in
        request.append(buffer)
    }
    engine.prepare()
    do { try engine.start() } catch { fail("The microphone didn't start: \(error.localizedDescription)") }
    _ = r.recognitionTask(with: request, resultHandler: handle)
    var stopping = false
    let stop = {
        if stopping { return }
        stopping = true
        engine.stop()
        input.removeTap(onBus: 0)
        request.endAudio()
        // The final result normally comes within a second.
        DispatchQueue.main.asyncAfter(deadline: .now() + 5) { finish() }
    }
    // oxbot closes stdin to stop.
    DispatchQueue.global().async {
        while readLine() != nil {}
        DispatchQueue.main.async { stop() }
    }
    DispatchQueue.main.asyncAfter(deadline: .now() + 120) { stop() }
}

func dictate(_ locale: String?, file: String?) -> Never {
    guard let r = recognizer(locale) else { fail("This Mac has no speech recognition for this language.") }
    guard r.supportsOnDeviceRecognition else { fail("On-device dictation isn't available on this Mac.") }
    authorize(mic: file == nil) {
        if let file = file { dictateFile(r, file) } else { dictateMic(r) }
    }
    dispatchMain()
}

// MARK: frames

func writeJPEG(_ image: CGImage, to url: URL) -> Bool {
    guard let dest = CGImageDestinationCreateWithURL(url as CFURL, "public.jpeg" as CFString, 1, nil) else {
        return false
    }
    CGImageDestinationAddImage(dest, image, [kCGImageDestinationLossyCompressionQuality: 0.8] as CFDictionary)
    return CGImageDestinationFinalize(dest)
}

func frames(_ path: String, _ outDir: String, max: Int, size: Int) async {
    let asset = AVURLAsset(url: URL(fileURLWithPath: path))
    let seconds: Double
    let tracks: [AVAssetTrack]
    do {
        seconds = CMTimeGetSeconds(try await asset.load(.duration))
        tracks = try await asset.loadTracks(withMediaType: .video)
    } catch {
        fail("This video can't be opened: \(error.localizedDescription)")
    }
    guard !tracks.isEmpty, seconds.isFinite, seconds > 0 else { fail("There's no picture in this video.") }
    // One a second for a short clip, evenly spaced, at most `max`.
    let count = Swift.max(1, Swift.min(max, Int(seconds.rounded(.up))))
    let generator = AVAssetImageGenerator(asset: asset)
    generator.appliesPreferredTrackTransform = true
    generator.maximumSize = CGSize(width: size, height: size)
    let tolerance = CMTime(seconds: Swift.min(0.5, seconds / Double(count) / 2), preferredTimescale: 600)
    generator.requestedTimeToleranceBefore = tolerance
    generator.requestedTimeToleranceAfter = tolerance
    do {
        try FileManager.default.createDirectory(atPath: outDir, withIntermediateDirectories: true)
    } catch {
        fail("Couldn't make the frames folder.")
    }
    var written = 0
    for i in 0..<count {
        let at = CMTime(seconds: (Double(i) + 0.5) * seconds / Double(count), preferredTimescale: 600)
        guard let (image, _) = try? await generator.image(at: at) else { continue }
        let name = String(format: "%02d.jpg", written + 1)
        if writeJPEG(image, to: URL(fileURLWithPath: outDir).appendingPathComponent(name)) {
            written += 1
        }
    }
    guard written > 0 else { fail("No still frames could be taken from this video.") }
    emit(["duration_ms": Int(seconds * 1000), "frames": written])
}

// MARK: main

let args = Array(CommandLine.arguments.dropFirst())
let words = plain(args)
switch words.first {
case "version":
    emit(["version": version])
case "can-dictate":
    canDictate(words.dropFirst().first)
case "dictate":
    dictate(words.dropFirst().first, file: option(args, "--file"))
case "frames":
    guard words.count >= 3 else { fail("usage: oxbot-mac frames VIDEO OUTDIR [--max 8] [--size 1024]") }
    let max = Int(option(args, "--max") ?? "") ?? 8
    let size = Int(option(args, "--size") ?? "") ?? 1024
    let (video, outDir) = (words[1], words[2])
    Task {
        await frames(video, outDir, max: Swift.min(Swift.max(max, 1), 30), size: Swift.min(Swift.max(size, 64), 4096))
        exit(0)
    }
    dispatchMain()
default:
    fail("usage: oxbot-mac version | can-dictate | dictate | frames")
}
