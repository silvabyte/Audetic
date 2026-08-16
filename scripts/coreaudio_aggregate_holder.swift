#!/usr/bin/env swift

import CoreAudio
import Darwin
import Dispatch
import Foundation

private struct HolderError: Error, CustomStringConvertible {
    let description: String
}

private let coreAudioSettleTimeout: TimeInterval = 5
private let coreAudioPollInterval: TimeInterval = 0.05

private func check(_ status: OSStatus, _ operation: String) throws {
    guard status == noErr else {
        throw HolderError(description: "\(operation) failed with OSStatus \(status)")
    }
}

private func address(_ selector: AudioObjectPropertySelector,
                     scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal)
    -> AudioObjectPropertyAddress
{
    AudioObjectPropertyAddress(
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain
    )
}

private func readScalar<T>(_ objectID: AudioObjectID,
                           _ selector: AudioObjectPropertySelector,
                           scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal,
                           initial: T) throws -> T
{
    var property = address(selector, scope: scope)
    var value = initial
    var size = UInt32(MemoryLayout<T>.size)
    let status = withUnsafeMutablePointer(to: &value) { pointer in
        AudioObjectGetPropertyData(objectID, &property, 0, nil, &size, pointer)
    }
    try check(status, "read CoreAudio property \(selector)")
    return value
}

private func readString(_ objectID: AudioObjectID,
                        _ selector: AudioObjectPropertySelector) throws -> String
{
    var property = address(selector)
    var value: CFString = "" as CFString
    var size = UInt32(MemoryLayout<CFString>.size)
    let status = withUnsafeMutablePointer(to: &value) { pointer in
        AudioObjectGetPropertyData(objectID, &property, 0, nil, &size, pointer)
    }
    try check(status, "read CoreAudio string property \(selector)")
    return value as String
}

private func deviceIDs() throws -> [AudioDeviceID] {
    var property = address(kAudioHardwarePropertyDevices)
    var size: UInt32 = 0
    try check(
        AudioObjectGetPropertyDataSize(
            AudioObjectID(kAudioObjectSystemObject), &property, 0, nil, &size
        ),
        "size CoreAudio device list"
    )
    var devices = [AudioDeviceID](
        repeating: 0,
        count: Int(size) / MemoryLayout<AudioDeviceID>.size
    )
    let status = devices.withUnsafeMutableBytes { bytes in
        AudioObjectGetPropertyData(
            AudioObjectID(kAudioObjectSystemObject), &property, 0, nil, &size,
            bytes.baseAddress!
        )
    }
    try check(status, "read CoreAudio device list")
    return devices
}

private func hasStreams(_ deviceID: AudioDeviceID,
                        scope: AudioObjectPropertyScope) -> Bool
{
    var property = address(kAudioDevicePropertyStreams, scope: scope)
    var size: UInt32 = 0
    return AudioObjectGetPropertyDataSize(deviceID, &property, 0, nil, &size) == noErr
        && size > 0
}

private func summary(_ deviceID: AudioDeviceID) throws -> [String: Any] {
    let rate: Float64 = try readScalar(
        deviceID, kAudioDevicePropertyNominalSampleRate, initial: 0
    )
    return [
        "device_id": deviceID,
        "uid": try readString(deviceID, kAudioDevicePropertyDeviceUID),
        "name": try readString(deviceID, kAudioObjectPropertyName),
        "nominal_rate_hz": rate,
        "has_input": hasStreams(deviceID, scope: kAudioDevicePropertyScopeInput),
        "has_output": hasStreams(deviceID, scope: kAudioDevicePropertyScopeOutput),
    ]
}

private func deviceID(uid: String) throws -> AudioDeviceID? {
    for deviceID in try deviceIDs() {
        if (try? readString(deviceID, kAudioDevicePropertyDeviceUID)) == uid {
            return deviceID
        }
    }
    return nil
}

private func findDevice(uid: String) throws -> AudioDeviceID {
    if let deviceID = try deviceID(uid: uid) {
        return deviceID
    }
    throw HolderError(description: "CoreAudio device UID not found: \(uid)")
}

private func defaultDevice(_ selector: AudioObjectPropertySelector) throws -> AudioDeviceID {
    try readScalar(
        AudioObjectID(kAudioObjectSystemObject), selector,
        initial: AudioDeviceID(kAudioObjectUnknown)
    )
}

private func waitForCoreAudio(_ operation: String,
                              condition: () throws -> Bool) throws
{
    let deadline = Date().addingTimeInterval(coreAudioSettleTimeout)
    repeat {
        if try condition() {
            return
        }
        Thread.sleep(forTimeInterval: coreAudioPollInterval)
    } while Date() < deadline
    throw HolderError(description: "Timed out waiting to \(operation)")
}

private func setSystemDefaultAndWait(_ selector: AudioObjectPropertySelector,
                                     deviceID: AudioDeviceID) throws
{
    var property = address(selector)
    var value = deviceID
    let status = withUnsafePointer(to: &value) { pointer in
        AudioObjectSetPropertyData(
            AudioObjectID(kAudioObjectSystemObject), &property, 0, nil,
            UInt32(MemoryLayout<AudioDeviceID>.size), pointer
        )
    }
    try check(status, "set CoreAudio default device")
    try waitForCoreAudio("read back default device \(deviceID)") {
        try defaultDevice(selector) == deviceID
    }
}

private func emit(_ value: [String: Any]) {
    do {
        let data = try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
        FileHandle.standardOutput.write(data)
        FileHandle.standardOutput.write(Data([0x0a]))
        fflush(stdout)
    } catch {
        fputs("{\"event\":\"error\",\"message\":\"JSON encoding failed\"}\n", stderr)
        fflush(stderr)
    }
}

private final class AggregateHolder {
    private let lock = NSLock()
    private var aggregateID: AudioDeviceID?
    private var cleaned = false
    private let uid: String
    let originalInput: AudioDeviceID
    let originalOutput: AudioDeviceID
    let originalSystemOutput: AudioDeviceID

    init(name: String, uid: String, inputUID: String?, outputUID: String?) throws {
        self.uid = uid
        originalInput = try defaultDevice(kAudioHardwarePropertyDefaultInputDevice)
        originalOutput = try defaultDevice(kAudioHardwarePropertyDefaultOutputDevice)
        originalSystemOutput = try defaultDevice(kAudioHardwarePropertyDefaultSystemOutputDevice)
        guard originalInput != AudioDeviceID(kAudioObjectUnknown),
              originalOutput != AudioDeviceID(kAudioObjectUnknown),
              originalSystemOutput != AudioDeviceID(kAudioObjectUnknown)
        else {
            throw HolderError(description: "CoreAudio defaults are unavailable; refusing a run that cannot restore them")
        }

        let resolvedInputUID = try inputUID ?? readString(
            originalInput, kAudioDevicePropertyDeviceUID
        )
        let resolvedOutputUID = try outputUID ?? readString(
            originalOutput, kAudioDevicePropertyDeviceUID
        )
        guard try deviceID(uid: uid) == nil else {
            throw HolderError(description: "Aggregate UID already exists: \(uid)")
        }
        let inputID = try findDevice(uid: resolvedInputUID)
        let outputID = try findDevice(uid: resolvedOutputUID)
        guard hasStreams(inputID, scope: kAudioDevicePropertyScopeInput) else {
            throw HolderError(description: "Input UID has no input streams: \(resolvedInputUID)")
        }
        guard hasStreams(outputID, scope: kAudioDevicePropertyScopeOutput) else {
            throw HolderError(description: "Output UID has no output streams: \(resolvedOutputUID)")
        }
        let inputSummary = try summary(inputID)
        let outputSummary = try summary(outputID)

        let orderedUIDs = resolvedInputUID == resolvedOutputUID
            ? [resolvedInputUID]
            : [resolvedInputUID, resolvedOutputUID]
        let subdevices: [[String: Any]] = orderedUIDs.enumerated().map { index, value in
            [
                kAudioSubDeviceUIDKey: value,
                kAudioSubDeviceDriftCompensationKey: index == 0 ? 0 : 1,
            ]
        }
        let description: [String: Any] = [
            kAudioAggregateDeviceNameKey: name,
            kAudioAggregateDeviceUIDKey: uid,
            kAudioAggregateDeviceSubDeviceListKey: subdevices,
            kAudioAggregateDeviceMainSubDeviceKey: orderedUIDs[0],
            kAudioAggregateDeviceIsPrivateKey: false,
            kAudioAggregateDeviceIsStackedKey: false,
        ]
        var created = AudioDeviceID(kAudioObjectUnknown)
        try check(
            AudioHardwareCreateAggregateDevice(description as CFDictionary, &created),
            "create public aggregate device"
        )
        do {
            try waitForCoreAudio("publish aggregate device \(uid)") {
                guard try deviceID(uid: uid) == created else { return false }
                return hasStreams(created, scope: kAudioDevicePropertyScopeInput)
                    && hasStreams(created, scope: kAudioDevicePropertyScopeOutput)
            }
        } catch {
            _ = AudioHardwareDestroyAggregateDevice(created)
            throw error
        }
        aggregateID = created

        emit([
            "event": "ready",
            "protocol": 1,
            "uid": uid,
            "name": name,
            "device_id": created,
            "original_default_input_id": originalInput,
            "original_default_output_id": originalOutput,
            "original_system_output_id": originalSystemOutput,
            "input": inputSummary,
            "output": outputSummary,
        ])
    }

    func setDefault(scope: String, deviceID: AudioDeviceID) throws {
        lock.lock()
        defer { lock.unlock() }
        guard !cleaned else {
            throw HolderError(description: "aggregate holder is already cleaned up")
        }
        switch scope {
        case "input":
            try setSystemDefaultAndWait(
                kAudioHardwarePropertyDefaultInputDevice, deviceID: deviceID
            )
        case "output":
            try setSystemDefaultAndWait(
                kAudioHardwarePropertyDefaultOutputDevice, deviceID: deviceID
            )
            try setSystemDefaultAndWait(
                kAudioHardwarePropertyDefaultSystemOutputDevice, deviceID: deviceID
            )
        default:
            throw HolderError(description: "scope must be input or output")
        }
    }

    func restore(scope: String?) throws {
        lock.lock()
        defer { lock.unlock() }
        try restoreUnlocked(scope: scope)
    }

    private func restoreUnlocked(scope: String?) throws {
        if scope == nil || scope == "input" {
            try setSystemDefaultAndWait(
                kAudioHardwarePropertyDefaultInputDevice, deviceID: originalInput
            )
        }
        if scope == nil || scope == "output" {
            try setSystemDefaultAndWait(
                kAudioHardwarePropertyDefaultOutputDevice, deviceID: originalOutput
            )
            try setSystemDefaultAndWait(
                kAudioHardwarePropertyDefaultSystemOutputDevice,
                deviceID: originalSystemOutput
            )
        }
    }

    @discardableResult
    func cleanup(reason: String) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard !cleaned else { return true }

        do { try restoreUnlocked(scope: nil) } catch {
            emit(["event": "warning", "message": "Default restoration failed: \(error)"])
        }
        if aggregateID != nil {
            do {
                if try deviceID(uid: uid) == nil {
                    aggregateID = nil
                }
            } catch {
                emit(["event": "warning", "message": "Failed to inspect aggregate before destroy: \(error)"])
            }
        }
        if let aggregateDeviceID = aggregateID {
            let status = AudioHardwareDestroyAggregateDevice(aggregateDeviceID)
            if status != noErr {
                emit([
                    "event": "error",
                    "message": "Aggregate destroy failed with OSStatus \(status)",
                ])
                return false
            }
            do {
                try waitForCoreAudio("observe aggregate device disappearance") {
                    try deviceID(uid: self.uid) == nil
                }
            } catch {
                emit(["event": "error", "message": "\(error)"])
                return false
            }
            aggregateID = nil
        }
        cleaned = true
        emit(["event": "destroyed", "reason": reason])
        return true
    }
}

private func argument(_ name: String, in arguments: [String]) -> String? {
    guard let index = arguments.firstIndex(of: name), index + 1 < arguments.count else {
        return nil
    }
    return arguments[index + 1]
}

private let arguments = Array(CommandLine.arguments.dropFirst())

if arguments.contains("--list") {
    do {
        emit(["event": "devices", "devices": try deviceIDs().compactMap { try? summary($0) }])
        exit(EXIT_SUCCESS)
    } catch {
        emit(["event": "error", "message": "\(error)"])
        exit(EXIT_FAILURE)
    }
}

guard let name = argument("--name", in: arguments),
      let uid = argument("--uid", in: arguments)
else {
    emit([
        "event": "error",
        "message": "usage: coreaudio_aggregate_holder.swift --name NAME --uid UUID [--input-uid UID] [--output-uid UID]",
    ])
    exit(EXIT_FAILURE)
}

do {
    let holder = try AggregateHolder(
        name: name,
        uid: uid,
        inputUID: argument("--input-uid", in: arguments),
        outputUID: argument("--output-uid", in: arguments)
    )

    signal(SIGPIPE, SIG_IGN)
    var signalSources: [DispatchSourceSignal] = []
    for signalNumber in [SIGINT, SIGTERM, SIGHUP] {
        signal(signalNumber, SIG_IGN)
        let source = DispatchSource.makeSignalSource(
            signal: signalNumber, queue: DispatchQueue.global()
        )
        source.setEventHandler {
            let cleaned = holder.cleanup(reason: "signal-\(signalNumber)")
            exit(cleaned ? 128 + signalNumber : EXIT_FAILURE)
        }
        source.resume()
        signalSources.append(source)
    }

    while let line = readLine() {
        do {
            guard let data = line.data(using: .utf8),
                  let command = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let verb = command["command"] as? String
            else {
                throw HolderError(description: "command must be a JSON object")
            }
            switch verb {
            case "set_default":
                guard let scope = command["scope"] as? String,
                      let number = command["device_id"] as? NSNumber
                else {
                    throw HolderError(description: "set_default needs scope and device_id")
                }
                try holder.setDefault(scope: scope, deviceID: number.uint32Value)
                emit([
                    "event": "ok",
                    "command": verb,
                    "scope": scope,
                    "device_id": number.uint32Value,
                ])
            case "restore":
                let scope = command["scope"] as? String
                try holder.restore(scope: scope)
                emit(["event": "ok", "command": verb, "scope": scope ?? "all"])
            case "destroy":
                if holder.cleanup(reason: "command") {
                    exit(EXIT_SUCCESS)
                }
            default:
                throw HolderError(description: "unknown command: \(verb)")
            }
        } catch {
            emit(["event": "error", "message": "\(error)"])
        }
    }

    _ = signalSources
    holder.cleanup(reason: "eof")
} catch {
    emit(["event": "error", "message": "\(error)"])
    exit(EXIT_FAILURE)
}
