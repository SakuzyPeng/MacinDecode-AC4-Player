// Isolate Control Center's content label from the PCM that actually reaches ASBR.
import SwiftUI
import AVFoundation
import AudioToolbox
import MediaToolbox
import Synchronization

private struct ProbeConfig: Decodable {
    var joc: String
    var pcm: String?
    var log: String
}

private final class TapCounters {
    let frames = Atomic<UInt64>(0)
    let channels = Atomic<UInt32>(0)
    let sampleRate = Atomic<UInt32>(0)
    let errors = Atomic<UInt64>(0)
}

@MainActor
private final class JOCPlayer {
    let player: AVPlayer
    let counters: TapCounters
    let silent: Bool
    private var endObserver: NSObjectProtocol?

    init(url: URL, silent: Bool) async throws {
        self.silent = silent
        counters = TapCounters()
        let asset = AVURLAsset(url: url)
        guard let track = try await asset.loadTracks(withMediaType: .audio).first else {
            throw NSError(domain: "Probe", code: 1, userInfo: [NSLocalizedDescriptionKey: "没有音轨"])
        }
        let item = AVPlayerItem(asset: asset)
        item.allowedAudioSpatializationFormats = .monoStereoAndMultichannel
        if silent {
            let retained = Unmanaged.passRetained(counters).toOpaque()
            var callbacks = MTAudioProcessingTapCallbacks(
                version: kMTAudioProcessingTapCallbacksVersion_0,
                clientInfo: retained,
                init: { _, info, storage in storage.pointee = info },
                finalize: { tap in
                    Unmanaged<TapCounters>.fromOpaque(MTAudioProcessingTapGetStorage(tap)).release()
                },
                prepare: { tap, _, asbd in
                    let state = Unmanaged<TapCounters>.fromOpaque(MTAudioProcessingTapGetStorage(tap)).takeUnretainedValue()
                    state.channels.store(asbd.pointee.mChannelsPerFrame, ordering: .relaxed)
                    state.sampleRate.store(UInt32(asbd.pointee.mSampleRate), ordering: .relaxed)
                },
                unprepare: { _ in },
                process: { tap, count, _, buffers, framesOut, flagsOut in
                    let state = Unmanaged<TapCounters>.fromOpaque(MTAudioProcessingTapGetStorage(tap)).takeUnretainedValue()
                    let status = MTAudioProcessingTapGetSourceAudio(tap, count, buffers, flagsOut, nil, framesOut)
                    if status != noErr {
                        state.errors.wrappingAdd(1, ordering: .relaxed)
                        framesOut.pointee = 0
                        return
                    }
                    // No PCM or metadata is forwarded from this player to the independent ASBR.
                    for buffer in UnsafeMutableAudioBufferListPointer(buffers) {
                        if let data = buffer.mData { memset(data, 0, Int(buffer.mDataByteSize)) }
                    }
                    state.frames.wrappingAdd(UInt64(framesOut.pointee), ordering: .relaxed)
                })
            var tap: MTAudioProcessingTap?
            let status = MTAudioProcessingTapCreate(kCFAllocatorDefault, &callbacks,
                kMTAudioProcessingTapCreationFlag_PostEffects, &tap)
            guard status == noErr, let tap else {
                Unmanaged<TapCounters>.fromOpaque(retained).release()
                throw NSError(domain: NSOSStatusErrorDomain, code: Int(status))
            }
            let input = AVMutableAudioMixInputParameters(track: track)
            input.audioTapProcessor = tap
            let mix = AVMutableAudioMix()
            mix.inputParameters = [input]
            item.audioMix = mix
        }
        player = AVPlayer(playerItem: item)
        // Match SpatialCompare's live decode + tap-zeroing method. Audible reference is quieter.
        player.volume = silent ? 1 : 0.12
        player.actionAtItemEnd = .none
        endObserver = NotificationCenter.default.addObserver(forName: .AVPlayerItemDidPlayToEndTime,
            object: item, queue: .main) { [weak player] _ in
                player?.seek(to: .zero) { [weak player] finished in
                    if finished { player?.play() }
                }
            }
    }

    func start() { player.play() }
    func stop() {
        if let endObserver { NotificationCenter.default.removeObserver(endObserver) }
        endObserver = nil
        player.pause()
        player.replaceCurrentItem(with: nil)
    }

    var diagnostic: [String: Any] {
        ["silent": silent, "tap_frames": counters.frames.load(ordering: .relaxed),
         "tap_channels": counters.channels.load(ordering: .relaxed),
         "tap_rate": counters.sampleRate.load(ordering: .relaxed),
         "tap_errors": counters.errors.load(ordering: .relaxed),
         "player_status": player.status.rawValue,
         "time_control": player.timeControlStatus.rawValue,
         "position": player.currentTime().seconds.isFinite ? player.currentTime().seconds : 0,
         "error": player.error?.localizedDescription ?? player.currentItem?.error?.localizedDescription ?? ""]
    }
}

private enum PCMLayout: String, CaseIterable {
    case atmos714 = "7.1.4"
    case atmos916 = "9.1.6"
    case cicp13 = "22.2"

    var channels: Int {
        switch self { case .atmos714: return 12; case .atmos916: return 16; case .cicp13: return 24 }
    }
    var tag: AudioChannelLayoutTag {
        switch self {
        case .atmos714: return kAudioChannelLayoutTag_Atmos_7_1_4
        case .atmos916: return kAudioChannelLayoutTag_Atmos_9_1_6
        case .cicp13: return kAudioChannelLayoutTag_CICP_13
        }
    }
    var filename: String {
        switch self { case .atmos714: return "ac4-714.caf"; case .atmos916: return "ac4-916.caf"; case .cicp13: return "ac4-222.caf" }
    }
}

private struct PCMAsset {
    let samples: [Float]
    let rate: Int32
    let name: String
    let layout: PCMLayout
    var channels: Int { layout.channels }

    static func tone() -> PCMAsset {
        let rate: Int32 = 48_000
        let channels = PCMLayout.atmos714.channels
        var samples = [Float](repeating: 0, count: Int(rate) * channels)
        for frame in 0..<Int(rate) {
            for channel in 0..<channels where channel != 3 {
                let frequency = Double(220 + channel * 31)
                samples[frame * channels + channel] = Float(0.002 * sin(2 * .pi * frequency * Double(frame) / Double(rate)))
            }
        }
        return PCMAsset(samples: samples, rate: rate, name: "独立合成 7.1.4 PCM", layout: .atmos714)
    }

    static func file(_ url: URL) throws -> PCMAsset {
        let file = try AVAudioFile(forReading: url, commonFormat: .pcmFormatFloat32, interleaved: true)
        let channels = Int(file.processingFormat.channelCount)
        let tag = file.fileFormat.channelLayout?.layoutTag
        guard let layout = PCMLayout.allCases.first(where: { $0.channels == channels && $0.tag == tag }) else {
            throw NSError(domain: "Probe", code: 2,
                userInfo: [NSLocalizedDescriptionKey: "需要带 7.1.4、9.1.6 或 22.2 布局标签的 PCM 文件"])
        }
        let frames = AVAudioFrameCount(min(file.length, Int64(file.processingFormat.sampleRate * 30)))
        guard let buffer = AVAudioPCMBuffer(pcmFormat: file.processingFormat, frameCapacity: frames) else {
            throw NSError(domain: "Probe", code: 3)
        }
        try file.read(into: buffer, frameCount: frames)
        guard buffer.frameLength > 0, let data = buffer.floatChannelData?[0] else {
            throw NSError(domain: "Probe", code: 4)
        }
        let samples = UnsafeBufferPointer(start: data, count: Int(buffer.frameLength) * channels).map { $0 * 0.12 }
        return PCMAsset(samples: samples, rate: Int32(file.processingFormat.sampleRate), name: url.lastPathComponent, layout: layout)
    }
}

private final class PCMPlayer {
    private let renderer = AVSampleBufferAudioRenderer()
    private let synchronizer = AVSampleBufferRenderSynchronizer()
    private let queue = DispatchQueue(label: "probe.independent-pcm")
    private var timer: DispatchSourceTimer?
    private var format: CMAudioFormatDescription!
    private let asset: PCMAsset
    private var submitted: Int64 = 0
    private var started = false
    private var failures = 0
    private var staging: [Float]

    init(asset: PCMAsset) throws {
        self.asset = asset
        staging = [Float](repeating: 0, count: 1024 * asset.channels)
        let bytesPerFrame = UInt32(asset.channels * MemoryLayout<Float>.size)
        var asbd = AudioStreamBasicDescription(mSampleRate: Double(asset.rate),
            mFormatID: kAudioFormatLinearPCM, mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
            mBytesPerPacket: bytesPerFrame, mFramesPerPacket: 1, mBytesPerFrame: bytesPerFrame,
            mChannelsPerFrame: UInt32(asset.channels), mBitsPerChannel: 32, mReserved: 0)
        var layout = AudioChannelLayout()
        layout.mChannelLayoutTag = asset.layout.tag
        let status = CMAudioFormatDescriptionCreate(allocator: kCFAllocatorDefault, asbd: &asbd,
            layoutSize: MemoryLayout<AudioChannelLayout>.size, layout: &layout,
            magicCookieSize: 0, magicCookie: nil, extensions: nil, formatDescriptionOut: &format)
        guard status == noErr else { throw NSError(domain: NSOSStatusErrorDomain, code: Int(status)) }
        renderer.allowedAudioSpatializationFormats = .monoStereoAndMultichannel
        synchronizer.addRenderer(renderer)
    }

    func start() {
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now(), repeating: .milliseconds(10), leeway: .milliseconds(2))
        timer.setEventHandler { [weak self] in self?.feed() }
        self.timer = timer
        timer.resume()
    }

    func stop() {
        timer?.cancel()
        timer = nil
        queue.sync {
            synchronizer.setRate(0, time: .zero)
            renderer.flush()
        }
    }

    private func feed() {
        let seconds = synchronizer.currentTime().seconds
        let played = started && seconds.isFinite ? Int64(seconds * Double(asset.rate)) : 0
        while renderer.isReadyForMoreMediaData, submitted - played < Int64(asset.rate) {
            let offset = Int(submitted) * asset.channels
            for i in staging.indices { staging[i] = asset.samples[(offset + i) % asset.samples.count] }
            var block: CMBlockBuffer?
            let bytes = staging.count * MemoryLayout<Float>.size
            guard CMBlockBufferCreateWithMemoryBlock(allocator: kCFAllocatorDefault,
                memoryBlock: nil, blockLength: bytes, blockAllocator: kCFAllocatorDefault,
                customBlockSource: nil, offsetToData: 0, dataLength: bytes, flags: 0,
                blockBufferOut: &block) == noErr, let block else { failures += 1; return }
            let copyStatus = staging.withUnsafeBytes { data in
                CMBlockBufferReplaceDataBytes(with: data.baseAddress!, blockBuffer: block,
                    offsetIntoDestination: 0, dataLength: bytes)
            }
            guard copyStatus == noErr else { failures += 1; return }
            var timing = CMSampleTimingInfo(duration: CMTime(value: 1, timescale: asset.rate),
                presentationTimeStamp: CMTime(value: submitted, timescale: asset.rate), decodeTimeStamp: .invalid)
            var buffer: CMSampleBuffer?
            guard CMSampleBufferCreate(allocator: kCFAllocatorDefault, dataBuffer: block,
                dataReady: true, makeDataReadyCallback: nil, refcon: nil, formatDescription: format,
                sampleCount: 1024, sampleTimingEntryCount: 1, sampleTimingArray: &timing,
                sampleSizeEntryCount: 0, sampleSizeArray: nil, sampleBufferOut: &buffer) == noErr,
                let buffer else { failures += 1; return }
            renderer.enqueue(buffer)
            submitted += 1024
            if !started, submitted >= Int64(Double(asset.rate) * 0.2) {
                started = true
                synchronizer.setRate(1, time: .zero)
            }
        }
    }

    var diagnostic: [String: Any] {
        queue.sync {
            let position = synchronizer.currentTime().seconds
            return ["source": asset.name, "submitted_frames": submitted,
             "channels": asset.channels, "sample_rate": asset.rate,
             "layout": asset.layout.rawValue, "layout_tag": String(format: "0x%08x", asset.layout.tag),
             "position": position.isFinite ? position : 0,
             "renderer_status": renderer.status.rawValue, "rate": synchronizer.rate,
             "failures": failures, "error": renderer.error?.localizedDescription ?? ""]
        }
    }
}

@MainActor
private final class Probe: ObservableObject {
    @Published var helperURL: URL?
    @Published var pcmURL: URL?
    @Published var useFile = false
    @Published var helperState = "已停止"
    @Published var pcmState = "已停止"
    @Published var status = ""
    @Published var busy = false
    @Published var helperRunning = false
    @Published var pcmRunning = false
    private var helper: JOCPlayer?
    private var pcm: PCMPlayer?
    private var timer: Timer?
    private var logFile: FileHandle?
    private var generation = 0

    init() {
        if let url = Bundle.main.url(forResource: "config", withExtension: "json"),
           let data = try? Data(contentsOf: url), let config = try? JSONDecoder().decode(ProbeConfig.self, from: data) {
            helperURL = URL(fileURLWithPath: config.joc)
            pcmURL = config.pcm.map { URL(fileURLWithPath: $0) }
            useFile = pcmURL != nil
            if !FileManager.default.fileExists(atPath: config.log) {
                FileManager.default.createFile(atPath: config.log, contents: nil)
            }
            logFile = try? FileHandle(forWritingTo: URL(fileURLWithPath: config.log))
            _ = try? logFile?.seekToEnd()
        }
        record("launch")
        timer = Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.refresh() }
        }
    }

    func pick(helper: Bool) {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        if panel.runModal() == .OK, let url = panel.url {
            if helper { helperURL = url } else { pcmURL = url; useFile = true }
        }
    }

    func startHelper(silent: Bool) {
        guard let url = helperURL else { return }
        stopHelper()
        let expected = generation
        busy = true
        Task {
            do {
                let player = try await JOCPlayer(url: url, silent: silent)
                guard expected == generation else { player.stop(); return }
                helper = player
                player.start()
                helperRunning = true
                helperState = silent ? "JOC 正在解码，tap 输出清零" : "JOC 有声参考"
                record(silent ? "start_silent_joc" : "start_direct_joc")
            } catch {
                status = error.localizedDescription
                record("helper_failed")
            }
            busy = false
        }
    }

    func stopHelper() {
        generation += 1
        helper?.stop()
        helper = nil
        helperRunning = false
        busy = false
        helperState = "已停止并释放 AVPlayerItem"
        record("stop_joc")
    }

    func startPCM() {
        stopPCM()
        do {
            let asset: PCMAsset
            if useFile {
                guard let pcmURL else { return }
                asset = try PCMAsset.file(pcmURL)
            } else { asset = .tone() }
            pcm = try PCMPlayer(asset: asset)
            pcm?.start()
            pcmRunning = true
            pcmState = "\(asset.layout.rawValue) · \(asset.channels) ch · \(asset.name)"
            record("start_pcm")
        } catch { status = error.localizedDescription; record("pcm_failed") }
    }

    func stopPCM() {
        pcm?.stop()
        pcm = nil
        pcmRunning = false
        pcmState = "已停止"
        record("stop_pcm")
    }

    func stopAll() { stopHelper(); stopPCM() }

    func runLayout(_ layout: PCMLayout) {
        guard let folder = pcmURL?.deletingLastPathComponent() else { return }
        pcmURL = folder.appendingPathComponent(layout.filename)
        useFile = true
        startPCM()
        if pcmRunning, helper == nil || helper?.silent == false { startHelper(silent: true) }
        refresh()
    }

    private func refresh() {
        let frames = helper?.counters.frames.load(ordering: .relaxed) ?? 0
        let channels = helper?.counters.channels.load(ordering: .relaxed) ?? 0
        let diagnostic = pcm?.diagnostic ?? [:]
        status = "JOC tap: \(channels) ch / \(frames) frames\nPCM: \(diagnostic["channels"] ?? 0) ch / \(diagnostic["submitted_frames"] ?? 0) frames"
        record("sample")
    }

    func record(_ event: String) {
        var entry: [String: Any] = ["event": event, "time": ISO8601DateFormatter().string(from: Date()),
            "pid": ProcessInfo.processInfo.processIdentifier,
            "helper_running": helperRunning, "pcm_running": pcmRunning]
        if let helper { entry["helper"] = helper.diagnostic }
        if let pcm { entry["pcm"] = pcm.diagnostic }
        if let data = try? JSONSerialization.data(withJSONObject: entry, options: [.sortedKeys]) {
            try? logFile?.write(contentsOf: data + Data([10]))
        }
    }
}

private struct ContentView: View {
    @StateObject private var model = Probe()
    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Atmos 标识隔离实验").font(.title2).bold()
            Text("辅助 JOC 与 PCM 输出互不传递数据，可分别启停。").foregroundStyle(.secondary)
            HStack {
                Button("选择 JOC…") { model.pick(helper: true) }
                Text(model.helperURL?.lastPathComponent ?? "未选择").lineLimit(1).truncationMode(.middle)
            }
            HStack {
                Button("启动静音 JOC") { model.startHelper(silent: true) }.disabled(model.busy)
                Button("启动有声 JOC 参考") { model.startHelper(silent: false) }.disabled(model.busy)
                Button("停止并释放 JOC") { model.stopHelper() }
            }
            Text(model.helperState)
            Divider()
            HStack {
                ForEach(PCMLayout.allCases, id: \.self) { layout in
                    Button("测试 \(layout.rawValue)") { model.runLayout(layout) }.disabled(model.busy)
                }
            }
            Toggle("使用 AC-4 渲染的 PCM 文件", isOn: $model.useFile).disabled(model.pcmRunning)
            HStack {
                Button("选择多声道 PCM…") { model.pick(helper: false) }.disabled(model.pcmRunning)
                Text(model.pcmURL?.lastPathComponent ?? "未选择；默认使用低电平合成音").lineLimit(1)
            }
            HStack {
                Button("启动独立 PCM") { model.startPCM() }.disabled(model.pcmRunning)
                Button("停止 PCM") { model.stopPCM() }
                Button("全部停止") { model.stopAll() }.keyboardShortcut(.space, modifiers: [])
            }
            Text(model.pcmState)
            Text(model.status).font(.system(.caption, design: .monospaced)).textSelection(.enabled)
            Text("文件 PCM 与有声 JOC 参考均降低约 18 dB；合成音为低电平。\n本程序不自动判断控制中心标识，请单独观察并记录。").font(.caption).foregroundStyle(.secondary)
        }
        .padding(20).frame(width: 620)
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.willTerminateNotification)) { _ in model.stopAll() }
    }
}

@main
struct AtmosBadgeProbeApp: App {
    var body: some Scene {
        WindowGroup("Atmos 标识隔离实验") { ContentView() }.windowResizability(.contentSize)
    }
}
