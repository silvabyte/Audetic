import CoreAudio
import Dispatch
import Foundation

private let helperVersion = 1
private let ownedUIDPrefix = "com.audetic.repro"

private enum Direction: String, Codable {
    case input
    case output

    var scope: AudioObjectPropertyScope {
        switch self {
        case .input: kAudioObjectPropertyScopeInput
        case .output: kAudioObjectPropertyScopeOutput
        }
    }

    var defaultSelector: AudioObjectPropertySelector {
        switch self {
        case .input: kAudioHardwarePropertyDefaultInputDevice
        case .output: kAudioHardwarePropertyDefaultOutputDevice
        }
    }
}

private struct Request: Codable {
    let id: String
    let op: String
    let name: String?
    let uid: String?
    let deviceUid: String?
    let direction: Direction?
    let rate: Double?
    let mono: Bool?
    let processPid: Int32?
    let muted: Bool?
}

private struct RateRangeRecord: Codable {
    let minimum: Double
    let maximum: Double
}

private struct DeviceRecord: Codable {
    let id: UInt32
    let uid: String
    let name: String
    let inputChannels: UInt32
    let outputChannels: UInt32
    let nominalRate: Double?
    let availableRates: [RateRangeRecord]
    let alive: Bool
    let defaultInput: Bool
    let defaultOutput: Bool
}

private struct ResourceRecord: Codable {
    let kind: String
    let id: UInt32
    let uid: String
    let name: String?
    let tapID: UInt32?
    let tapUID: String?
    let physicalDeviceUID: String?
    let direction: Direction?
    let nominalRate: Double?
}

private struct ResultPayload: Codable {
    var protocolVersion: Int?
    var helper: String?
    var capabilities: [String]?
    var devices: [DeviceRecord]?
    var defaultInputUID: String?
    var defaultOutputUID: String?
    var resource: ResourceRecord?
    var uid: String?
    var direction: Direction?
    var rate: Double?
    var restored: Bool?
    var destroyed: Bool?
    var shuttingDown: Bool?
}

private struct ErrorPayload: Codable, Error {
    let code: String
    let message: String
    let osStatus: Int32?
    let fourcc: String?

    init(code: String, message: String, status: OSStatus? = nil) {
        self.code = code
        self.message = message
        osStatus = status
        fourcc = status.flatMap(statusFourCC)
    }
}

private struct Response: Codable {
    let type: String
    let id: String?
    let op: String?
    let ok: Bool
    let result: ResultPayload?
    let error: ErrorPayload?

    static func success(id: String, op: String, result: ResultPayload) -> Response {
        Response(type: "response", id: id, op: op, ok: true, result: result, error: nil)
    }

    static func failure(id: String?, op: String?, error: ErrorPayload) -> Response {
        Response(type: "response", id: id, op: op, ok: false, result: nil, error: error)
    }
}

private func statusFourCC(_ status: OSStatus) -> String? {
    let value = UInt32(bitPattern: status)
    let bytes = [
        UInt8((value >> 24) & 0xff),
        UInt8((value >> 16) & 0xff),
        UInt8((value >> 8) & 0xff),
        UInt8(value & 0xff),
    ]
    guard bytes.allSatisfy({ (32 ... 126).contains($0) }) else {
        return nil
    }
    return String(bytes: bytes, encoding: .ascii)
}

private func coreAudioError(_ operation: String, _ status: OSStatus) -> ErrorPayload {
    ErrorPayload(
        code: "coreaudio_error",
        message: "\(operation) failed",
        status: status
    )
}

private func propertyAddress(
    _ selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal
) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress(
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain
    )
}

private func scalarProperty<T>(
    objectID: AudioObjectID,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal,
    operation: String
) throws -> T {
    var address = propertyAddress(selector, scope: scope)
    var size = UInt32(MemoryLayout<T>.size)
    let value = UnsafeMutablePointer<T>.allocate(capacity: 1)
    defer { value.deallocate() }
    let status = AudioObjectGetPropertyData(objectID, &address, 0, nil, &size, value)
    guard status == noErr else {
        throw coreAudioError(operation, status)
    }
    guard size == UInt32(MemoryLayout<T>.size) else {
        throw ErrorPayload(code: "invalid_property", message: "\(operation) returned an unexpected size")
    }
    return value.pointee
}

private func stringProperty(
    objectID: AudioObjectID,
    selector: AudioObjectPropertySelector,
    operation: String
) throws -> String {
    var address = propertyAddress(selector)
    var size = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
    var value: Unmanaged<CFString>?
    let status = withUnsafeMutablePointer(to: &value) {
        AudioObjectGetPropertyData(objectID, &address, 0, nil, &size, $0)
    }
    guard status == noErr else {
        throw coreAudioError(operation, status)
    }
    guard let value else {
        throw ErrorPayload(code: "invalid_property", message: "\(operation) returned no string")
    }
    return value.takeRetainedValue() as String
}

private func setScalarProperty<T>(
    objectID: AudioObjectID,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal,
    value: T,
    operation: String
) throws {
    var address = propertyAddress(selector, scope: scope)
    var value = value
    let status = withUnsafePointer(to: &value) {
        AudioObjectSetPropertyData(
            objectID,
            &address,
            0,
            nil,
            UInt32(MemoryLayout<T>.size),
            $0
        )
    }
    guard status == noErr else {
        throw coreAudioError(operation, status)
    }
}

private func setStringArrayProperty(
    objectID: AudioObjectID,
    selector: AudioObjectPropertySelector,
    values: [String],
    operation: String
) throws {
    var address = propertyAddress(selector)
    var value = values as CFArray
    let status = withUnsafePointer(to: &value) {
        AudioObjectSetPropertyData(
            objectID,
            &address,
            0,
            nil,
            UInt32(MemoryLayout<CFArray>.size),
            $0
        )
    }
    guard status == noErr else {
        throw coreAudioError(operation, status)
    }
}

private func arrayProperty<T>(
    objectID: AudioObjectID,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal,
    operation: String
) throws -> [T] {
    var address = propertyAddress(selector, scope: scope)
    var size: UInt32 = 0
    var status = AudioObjectGetPropertyDataSize(objectID, &address, 0, nil, &size)
    guard status == noErr else {
        throw coreAudioError("\(operation) size", status)
    }
    guard size % UInt32(MemoryLayout<T>.stride) == 0 else {
        throw ErrorPayload(code: "invalid_property", message: "\(operation) returned an invalid array size")
    }
    let count = Int(size) / MemoryLayout<T>.stride
    guard count > 0 else {
        return []
    }
    let values = UnsafeMutablePointer<T>.allocate(capacity: count)
    defer { values.deallocate() }
    status = AudioObjectGetPropertyData(objectID, &address, 0, nil, &size, values)
    guard status == noErr else {
        throw coreAudioError(operation, status)
    }
    guard size % UInt32(MemoryLayout<T>.stride) == 0 else {
        throw ErrorPayload(code: "invalid_property", message: "\(operation) returned an invalid final array size")
    }
    let returnedCount = Int(size) / MemoryLayout<T>.stride
    guard returnedCount <= count else {
        throw ErrorPayload(code: "property_grew", message: "\(operation) grew while being read")
    }
    return Array(UnsafeBufferPointer(start: values, count: returnedCount))
}

private func allDeviceIDs() throws -> [AudioObjectID] {
    try arrayProperty(
        objectID: AudioObjectID(kAudioObjectSystemObject),
        selector: kAudioHardwarePropertyDevices,
        operation: "list audio devices"
    )
}

private func deviceUID(_ id: AudioObjectID) throws -> String {
    try stringProperty(objectID: id, selector: kAudioDevicePropertyDeviceUID, operation: "read device UID")
}

private func deviceName(_ id: AudioObjectID) throws -> String {
    try stringProperty(objectID: id, selector: kAudioObjectPropertyName, operation: "read device name")
}

private func currentDefaultDeviceID(_ direction: Direction) throws -> AudioObjectID {
    try scalarProperty(
        objectID: AudioObjectID(kAudioObjectSystemObject),
        selector: direction.defaultSelector,
        operation: "read default \(direction.rawValue) device"
    )
}

private func nominalRate(_ id: AudioObjectID) throws -> Double {
    try scalarProperty(
        objectID: id,
        selector: kAudioDevicePropertyNominalSampleRate,
        operation: "read nominal sample rate"
    )
}

private func availableRates(_ id: AudioObjectID) throws -> [AudioValueRange] {
    try arrayProperty(
        objectID: id,
        selector: kAudioDevicePropertyAvailableNominalSampleRates,
        operation: "read available nominal sample rates"
    )
}

private func channelCount(_ id: AudioObjectID, direction: Direction) throws -> UInt32 {
    var address = propertyAddress(kAudioDevicePropertyStreamConfiguration, scope: direction.scope)
    var size: UInt32 = 0
    var status = AudioObjectGetPropertyDataSize(id, &address, 0, nil, &size)
    guard status == noErr else {
        throw coreAudioError("read \(direction.rawValue) channel layout size", status)
    }
    guard size >= UInt32(MemoryLayout<AudioBufferList>.size) else {
        return 0
    }
    let storage = UnsafeMutableRawPointer.allocate(
        byteCount: Int(size),
        alignment: MemoryLayout<AudioBufferList>.alignment
    )
    defer { storage.deallocate() }
    status = AudioObjectGetPropertyData(id, &address, 0, nil, &size, storage)
    guard status == noErr else {
        throw coreAudioError("read \(direction.rawValue) channel layout", status)
    }
    let list = storage.assumingMemoryBound(to: AudioBufferList.self)
    return UnsafeMutableAudioBufferListPointer(list).reduce(0) { $0 + $1.mNumberChannels }
}

private final class OwnedResource {
    enum Kind {
        case aggregate(uid: String, name: String, physicalDeviceUID: String?, direction: Direction?)
        case tap(uuid: String, aggregateUID: String)
    }

    let id: AudioObjectID
    let kind: Kind
    var active = true

    init(id: AudioObjectID, kind: Kind) {
        self.id = id
        self.kind = kind
    }
}

private struct OriginalRate {
    let uid: String
    let idAtTouch: AudioObjectID
    let rate: Double
}

@available(macOS 14.6, *)
private final class AudioDeviceManager {
    private let originalDefaults: [Direction: String]
    private let startupRates: [String: Double]
    private var touchedDefaults: Set<Direction> = []
    private var lastSetDefaults: [Direction: String] = [:]
    private var touchedRates: [OriginalRate] = []
    private var touchedRateUIDs: Set<String> = []
    private var lastSetRates: [String: Double] = [:]
    private var resources: [OwnedResource] = []
    private var hoggedDeviceIDs: Set<AudioObjectID> = []
    private var cleanupComplete = false

    init() {
        var defaults: [Direction: String] = [:]
        for direction in [Direction.input, .output] {
            if let id = try? currentDefaultDeviceID(direction),
               id != kAudioObjectUnknown,
               let uid = try? deviceUID(id)
            {
                defaults[direction] = uid
            }
        }
        originalDefaults = defaults

        var rates: [String: Double] = [:]
        if let ids = try? allDeviceIDs() {
            for id in ids {
                if let uid = try? deviceUID(id), let rate = try? nominalRate(id) {
                    rates[uid] = rate
                }
            }
        }
        startupRates = rates
    }

    func handle(_ request: Request) -> (Response, Bool) {
        do {
            switch request.op {
            case "hello":
                return (.success(
                    id: request.id,
                    op: request.op,
                    result: ResultPayload(
                        protocolVersion: helperVersion,
                        helper: "audiodev",
                        capabilities: [
                            "hello",
                            "snapshot",
                            "list_devices",
                            "create_tap_aggregate",
                            "create_subdevice_aggregate",
                            "set_default",
                            "force_owned_default",
                            "set_rate",
                            "hog_aggregate",
                            "release_hog",
                            "break_aggregate",
                            "destroy_tap",
                            "destroy_aggregate",
                            "restore",
                            "shutdown",
                        ]
                    )
                ), false)
            case "snapshot", "list_devices":
                return (.success(id: request.id, op: request.op, result: try snapshot()), false)
            case "create_tap_aggregate":
                return (.success(id: request.id, op: request.op, result: try createTapAggregate(request)), false)
            case "create_subdevice_aggregate":
                return (.success(id: request.id, op: request.op, result: try createSubdeviceAggregate(request)), false)
            case "set_default":
                return (.success(id: request.id, op: request.op, result: try setDefault(request)), false)
            case "force_owned_default":
                return (.success(id: request.id, op: request.op, result: try forceOwnedDefault(request)), false)
            case "set_rate":
                return (.success(id: request.id, op: request.op, result: try setRate(request)), false)
            case "hog_aggregate":
                return (.success(id: request.id, op: request.op, result: try hogAggregate(request)), false)
            case "release_hog":
                return (.success(id: request.id, op: request.op, result: try releaseHog(request)), false)
            case "break_aggregate":
                return (.success(id: request.id, op: request.op, result: try breakAggregate(request)), false)
            case "destroy_tap":
                return (.success(id: request.id, op: request.op, result: try destroyTap(request)), false)
            case "destroy_aggregate":
                return (.success(id: request.id, op: request.op, result: try destroyAggregate(request)), false)
            case "restore":
                try explicitCleanup()
                return (.success(
                    id: request.id,
                    op: request.op,
                    result: ResultPayload(restored: true)
                ), false)
            case "shutdown":
                try explicitCleanup()
                return (.success(
                    id: request.id,
                    op: request.op,
                    result: ResultPayload(restored: true, shuttingDown: true)
                ), true)
            default:
                throw ErrorPayload(code: "unsupported_op", message: "unsupported operation: \(request.op)")
            }
        } catch let error as ErrorPayload {
            return (.failure(id: request.id, op: request.op, error: error), request.op == "shutdown")
        } catch {
            return (.failure(
                id: request.id,
                op: request.op,
                error: ErrorPayload(code: "internal_error", message: String(describing: error))
            ), request.op == "shutdown")
        }
    }

    func cleanupErrors() -> [String] {
        performCleanup().map { "\($0.code): \($0.message)" }
    }

    private func snapshot() throws -> ResultPayload {
        let inputID = try? defaultDeviceID(.input)
        let outputID = try? defaultDeviceID(.output)
        let devices = try allDeviceIDs().compactMap { id -> DeviceRecord? in
            guard let uid = try? deviceUID(id), let name = try? deviceName(id) else {
                return nil
            }
            let inputChannels = (try? channelCount(id, direction: .input)) ?? 0
            let outputChannels = (try? channelCount(id, direction: .output)) ?? 0
            let rate = try? nominalRate(id)
            let ranges = ((try? availableRates(id)) ?? [])
                .filter { $0.mMinimum.isFinite && $0.mMaximum.isFinite }
                .map { RateRangeRecord(minimum: $0.mMinimum, maximum: $0.mMaximum) }
            let alive: UInt32
            do {
                alive = try scalarProperty(
                    objectID: id,
                    selector: kAudioDevicePropertyDeviceIsAlive,
                    operation: "read device alive state"
                )
            } catch {
                alive = 0
            }
            return DeviceRecord(
                id: id,
                uid: uid,
                name: name,
                inputChannels: inputChannels,
                outputChannels: outputChannels,
                nominalRate: rate,
                availableRates: ranges,
                alive: alive != 0,
                defaultInput: id == inputID,
                defaultOutput: id == outputID
            )
        }
        return ResultPayload(
            devices: devices.sorted { lhs, rhs in
                lhs.name == rhs.name ? lhs.uid < rhs.uid : lhs.name.localizedStandardCompare(rhs.name) == .orderedAscending
            },
            defaultInputUID: inputID.flatMap { try? deviceUID($0) },
            defaultOutputUID: outputID.flatMap { try? deviceUID($0) }
        )
    }

    private func createTapAggregate(_ request: Request) throws -> ResultPayload {
        let (name, uid) = try creationIdentity(request)
        let mono = request.mono ?? false
        let physicalDeviceUID = request.deviceUid
        if let rate = request.rate {
            try validateRate(rate)
        }
        if let physicalDeviceUID {
            let physicalID = try resolveCurrentDevice(uid: physicalDeviceUID)
            guard try channelCount(physicalID, direction: .output) > 0 else {
                throw ErrorPayload(
                    code: "direction_unavailable",
                    message: "tap aggregate backing device has no output channels"
                )
            }
        }
        cleanupComplete = false

        let tapDescription: CATapDescription
        if let processPID = request.processPid {
            let processObject = try translateProcessID(processPID)
            tapDescription = mono
                ? CATapDescription(monoMixdownOfProcesses: [processObject])
                : CATapDescription(stereoMixdownOfProcesses: [processObject])
        } else {
            tapDescription = mono
                ? CATapDescription(monoGlobalTapButExcludeProcesses: [])
                : CATapDescription(stereoGlobalTapButExcludeProcesses: [])
        }
        let tapUUID = UUID()
        tapDescription.name = name
        tapDescription.uuid = tapUUID
        tapDescription.isPrivate = false
        tapDescription.muteBehavior = request.muted == true ? .muted : .unmuted

        var tapID = AudioObjectID(kAudioObjectUnknown)
        var status = AudioHardwareCreateProcessTap(tapDescription, &tapID)
        guard status == noErr else {
            throw coreAudioError("create process tap", status)
        }
        let tapResource = OwnedResource(
            id: tapID,
            kind: .tap(uuid: tapUUID.uuidString, aggregateUID: uid)
        )
        resources.append(tapResource)

        var aggregateID = AudioObjectID(kAudioObjectUnknown)
        var description: [String: Any] = [
            kAudioAggregateDeviceNameKey: name,
            kAudioAggregateDeviceUIDKey: uid,
            kAudioAggregateDeviceIsPrivateKey: false,
            kAudioAggregateDeviceTapListKey: [[kAudioSubTapUIDKey: tapUUID.uuidString]],
        ]
        if let physicalDeviceUID {
            description[kAudioAggregateDeviceMainSubDeviceKey] = physicalDeviceUID
            description[kAudioAggregateDeviceSubDeviceListKey] = [[
                kAudioSubDeviceUIDKey: physicalDeviceUID,
                kAudioSubDeviceDriftCompensationKey: false,
            ]]
        }
        status = AudioHardwareCreateAggregateDevice(description as CFDictionary, &aggregateID)
        guard status == noErr else {
            _ = destroyOwnedResource(tapResource)
            throw coreAudioError("create tap aggregate device", status)
        }
        let aggregateResource = OwnedResource(
            id: aggregateID,
            kind: .aggregate(
                uid: uid,
                name: name,
                physicalDeviceUID: physicalDeviceUID,
                direction: .input
            )
        )
        resources.append(aggregateResource)

        do {
            try waitForDevice(uid: uid, expectedID: aggregateID)
            if let rate = request.rate {
                if let physicalDeviceUID {
                    let physicalID = try resolveCurrentDevice(uid: physicalDeviceUID)
                    try rememberRate(uid: physicalDeviceUID, id: physicalID)
                }
                try setNominalRate(
                    id: aggregateID,
                    uid: uid,
                    rate: rate,
                    physicalDeviceUID: physicalDeviceUID
                )
            }
            return ResultPayload(resource: try resourceRecord(aggregateResource, tap: tapResource))
        } catch {
            _ = destroyOwnedResource(aggregateResource)
            _ = destroyOwnedResource(tapResource)
            throw error
        }
    }

    private func createSubdeviceAggregate(_ request: Request) throws -> ResultPayload {
        let (name, uid) = try creationIdentity(request)
        guard let physicalUID = request.deviceUid, !physicalUID.isEmpty else {
            throw ErrorPayload(code: "invalid_request", message: "device_uid is required")
        }
        guard let direction = request.direction else {
            throw ErrorPayload(code: "invalid_request", message: "direction is required")
        }
        if let rate = request.rate {
            try validateRate(rate)
        }

        let physicalID = try resolveCurrentDevice(uid: physicalUID)
        let objectClass: AudioClassID = try scalarProperty(
            objectID: physicalID,
            selector: kAudioObjectPropertyClass,
            operation: "read physical device class"
        )
        guard objectClass != kAudioAggregateDeviceClassID else {
            throw ErrorPayload(code: "invalid_device", message: "device_uid must identify a non-aggregate device")
        }
        let transportType: UInt32 = try scalarProperty(
            objectID: physicalID,
            selector: kAudioDevicePropertyTransportType,
            operation: "read physical device transport type"
        )
        guard transportType != kAudioDeviceTransportTypeVirtual else {
            throw ErrorPayload(code: "invalid_device", message: "device_uid must identify a physical device")
        }
        guard try channelCount(physicalID, direction: direction) > 0 else {
            throw ErrorPayload(
                code: "direction_unavailable",
                message: "device has no \(direction.rawValue) channels"
            )
        }
        cleanupComplete = false

        let description: [String: Any] = [
            kAudioAggregateDeviceNameKey: name,
            kAudioAggregateDeviceUIDKey: uid,
            kAudioAggregateDeviceIsPrivateKey: false,
            kAudioAggregateDeviceIsStackedKey: false,
            kAudioAggregateDeviceMainSubDeviceKey: physicalUID,
            kAudioAggregateDeviceSubDeviceListKey: [[
                kAudioSubDeviceUIDKey: physicalUID,
                kAudioSubDeviceDriftCompensationKey: false,
            ]],
        ]
        var aggregateID = AudioObjectID(kAudioObjectUnknown)
        let status = AudioHardwareCreateAggregateDevice(description as CFDictionary, &aggregateID)
        guard status == noErr else {
            throw coreAudioError("create subdevice aggregate device", status)
        }
        let aggregateResource = OwnedResource(
            id: aggregateID,
            kind: .aggregate(
                uid: uid,
                name: name,
                physicalDeviceUID: physicalUID,
                direction: direction
            )
        )
        resources.append(aggregateResource)

        do {
            try waitForDevice(uid: uid, expectedID: aggregateID)
            guard try channelCount(aggregateID, direction: direction) > 0 else {
                throw ErrorPayload(
                    code: "direction_unavailable",
                    message: "created aggregate has no \(direction.rawValue) channels"
                )
            }
            if let rate = request.rate {
                try rememberRate(uid: physicalUID, id: physicalID)
                try setNominalRate(
                    id: aggregateID,
                    uid: uid,
                    rate: rate,
                    physicalDeviceUID: physicalUID
                )
            }
            return ResultPayload(resource: try resourceRecord(aggregateResource, tap: nil))
        } catch {
            _ = destroyOwnedResource(aggregateResource)
            throw error
        }
    }

    private func setDefault(_ request: Request) throws -> ResultPayload {
        guard let uid = request.uid, !uid.isEmpty else {
            throw ErrorPayload(code: "invalid_request", message: "uid is required")
        }
        guard let direction = request.direction else {
            throw ErrorPayload(code: "invalid_request", message: "direction is required")
        }
        let id = try resolveCurrentDevice(uid: uid)
        guard try channelCount(id, direction: direction) > 0 else {
            throw ErrorPayload(
                code: "direction_unavailable",
                message: "device has no \(direction.rawValue) channels"
            )
        }
        guard originalDefaults[direction] != nil else {
            throw ErrorPayload(
                code: "restore_unavailable",
                message: "startup default \(direction.rawValue) device was unavailable"
            )
        }
        let previous = try defaultDeviceID(direction)
        if previous == id {
            return ResultPayload(uid: uid, direction: direction)
        }
        cleanupComplete = false
        touchedDefaults.insert(direction)
        lastSetDefaults[direction] = uid
        try setScalarProperty(
            objectID: AudioObjectID(kAudioObjectSystemObject),
            selector: direction.defaultSelector,
            value: id,
            operation: "set default \(direction.rawValue) device"
        )
        try waitForDefault(direction, expectedID: id)
        return ResultPayload(uid: uid, direction: direction)
    }

    private func setRate(_ request: Request) throws -> ResultPayload {
        guard let uid = request.uid, !uid.isEmpty else {
            throw ErrorPayload(code: "invalid_request", message: "uid is required")
        }
        guard let rate = request.rate else {
            throw ErrorPayload(code: "invalid_request", message: "rate is required")
        }
        try validateRate(rate)
        let id = try resolveCurrentDevice(uid: uid)
        cleanupComplete = false
        let physicalUID = activeAggregate(uid: uid).flatMap { resource -> String? in
            guard case let .aggregate(_, _, physicalDeviceUID, _) = resource.kind else {
                return nil
            }
            return physicalDeviceUID
        }
        try setNominalRate(id: id, uid: uid, rate: rate, physicalDeviceUID: physicalUID)
        return ResultPayload(uid: uid, rate: try nominalRate(id))
    }

    private func forceOwnedDefault(_ request: Request) throws -> ResultPayload {
        guard let uid = request.uid, !uid.isEmpty else {
            throw ErrorPayload(code: "invalid_request", message: "uid is required")
        }
        guard let direction = request.direction else {
            throw ErrorPayload(code: "invalid_request", message: "direction is required")
        }
        guard let resource = activeAggregate(uid: uid) else {
            throw ErrorPayload(code: "not_owned", message: "no owned active aggregate for UID \(uid)")
        }
        guard case let .aggregate(_, _, _, ownedDirection) = resource.kind,
              ownedDirection == direction
        else {
            throw ErrorPayload(
                code: "direction_unavailable",
                message: "owned aggregate was not created for \(direction.rawValue)"
            )
        }
        guard originalDefaults[direction] != nil else {
            throw ErrorPayload(
                code: "restore_unavailable",
                message: "startup default \(direction.rawValue) device was unavailable"
            )
        }
        cleanupComplete = false
        touchedDefaults.insert(direction)
        lastSetDefaults[direction] = uid
        try setScalarProperty(
            objectID: AudioObjectID(kAudioObjectSystemObject),
            selector: direction.defaultSelector,
            value: resource.id,
            operation: "force owned default \(direction.rawValue) device"
        )
        try waitForDefault(direction, expectedID: resource.id)
        return ResultPayload(uid: uid, direction: direction)
    }

    private func hogAggregate(_ request: Request) throws -> ResultPayload {
        let resource = try requestedOwnedAggregate(request)
        let deviceID = try hogTarget(for: resource)
        var hogOwner = pid_t(getpid())
        try setScalarProperty(
            objectID: deviceID,
            selector: kAudioDevicePropertyHogMode,
            value: hogOwner,
            operation: "take aggregate hog mode"
        )
        hogOwner = try scalarProperty(
            objectID: deviceID,
            selector: kAudioDevicePropertyHogMode,
            operation: "verify aggregate hog mode"
        )
        guard hogOwner == pid_t(getpid()) else {
            throw ErrorPayload(
                code: "hog_not_acquired",
                message: "aggregate hog mode is owned by PID \(hogOwner), not this holder"
            )
        }
        cleanupComplete = false
        hoggedDeviceIDs.insert(deviceID)
        return ResultPayload(uid: request.uid)
    }

    private func releaseHog(_ request: Request) throws -> ResultPayload {
        let resource = try requestedOwnedAggregate(request)
        try releaseHog(deviceID: try hogTarget(for: resource))
        return ResultPayload(uid: request.uid)
    }

    private func hogTarget(for resource: OwnedResource) throws -> AudioObjectID {
        guard case let .aggregate(_, _, physicalDeviceUID, _) = resource.kind else {
            throw ErrorPayload(code: "not_owned", message: "owned resource is not an aggregate")
        }
        if let physicalDeviceUID {
            return try resolveCurrentDevice(uid: physicalDeviceUID)
        }
        return resource.id
    }

    private func requestedOwnedAggregate(_ request: Request) throws -> OwnedResource {
        guard let uid = request.uid, !uid.isEmpty else {
            throw ErrorPayload(code: "invalid_request", message: "uid is required")
        }
        guard let resource = activeAggregate(uid: uid) else {
            throw ErrorPayload(code: "not_owned", message: "no owned active aggregate for UID \(uid)")
        }
        return resource
    }

    private func releaseHog(deviceID: AudioObjectID) throws {
        guard hoggedDeviceIDs.contains(deviceID) else {
            return
        }
        let nobody = pid_t(-1)
        try setScalarProperty(
            objectID: deviceID,
            selector: kAudioDevicePropertyHogMode,
            value: nobody,
            operation: "release aggregate hog mode"
        )
        let owner: pid_t = try scalarProperty(
            objectID: deviceID,
            selector: kAudioDevicePropertyHogMode,
            operation: "verify released aggregate hog mode"
        )
        guard owner == nobody else {
            throw ErrorPayload(
                code: "hog_not_released",
                message: "aggregate hog mode remained owned by PID \(owner)"
            )
        }
        hoggedDeviceIDs.remove(deviceID)
    }

    private func destroyTap(_ request: Request) throws -> ResultPayload {
        guard let aggregateUID = request.uid, !aggregateUID.isEmpty else {
            throw ErrorPayload(code: "invalid_request", message: "uid is required")
        }
        guard let resource = resources.reversed().first(where: { resource in
            guard resource.active, case let .tap(_, ownerUID) = resource.kind else {
                return false
            }
            return ownerUID == aggregateUID
        }) else {
            throw ErrorPayload(code: "not_owned", message: "no owned active tap for UID \(aggregateUID)")
        }
        if let error = destroyOwnedResource(resource) {
            throw error
        }
        return ResultPayload(uid: aggregateUID, destroyed: true)
    }

    private func breakAggregate(_ request: Request) throws -> ResultPayload {
        guard let uid = request.uid, !uid.isEmpty else {
            throw ErrorPayload(code: "invalid_request", message: "uid is required")
        }
        guard let resource = activeAggregate(uid: uid) else {
            throw ErrorPayload(code: "not_owned", message: "no owned active aggregate for UID \(uid)")
        }
        guard case .aggregate = resource.kind else {
            throw ErrorPayload(code: "not_owned", message: "owned resource is not an aggregate")
        }
        cleanupComplete = false
        try setStringArrayProperty(
            objectID: resource.id,
            selector: kAudioAggregateDevicePropertyFullSubDeviceList,
            values: [],
            operation: "remove aggregate subdevices"
        )
        let deadline = Date().addingTimeInterval(2)
        repeat {
            if (try? channelCount(resource.id, direction: .input)) == 0,
               (try? channelCount(resource.id, direction: .output)) == 0
            {
                return ResultPayload(uid: uid)
            }
            usleep(20_000)
        } while Date() < deadline
        throw ErrorPayload(
            code: "aggregate_not_broken",
            message: "aggregate retained active channels after its subdevice list was cleared"
        )
    }

    private func destroyAggregate(_ request: Request) throws -> ResultPayload {
        guard let uid = request.uid, !uid.isEmpty else {
            throw ErrorPayload(code: "invalid_request", message: "uid is required")
        }
        guard let resource = activeAggregate(uid: uid) else {
            throw ErrorPayload(code: "not_owned", message: "no owned active aggregate for UID \(uid)")
        }
        if let error = destroyOwnedResource(resource) {
            throw error
        }
        return ResultPayload(uid: uid, destroyed: true)
    }

    private func creationIdentity(_ request: Request) throws -> (String, String) {
        guard let name = request.name?.trimmingCharacters(in: .whitespacesAndNewlines), !name.isEmpty else {
            throw ErrorPayload(code: "invalid_request", message: "name is required")
        }
        guard let uid = request.uid, uid.starts(with: ownedUIDPrefix) else {
            throw ErrorPayload(
                code: "invalid_uid",
                message: "uid must start with \(ownedUIDPrefix)"
            )
        }
        let ids = try allDeviceIDs()
        for id in ids {
            if (try? deviceUID(id)) == uid {
                throw ErrorPayload(code: "duplicate_uid", message: "a device with UID \(uid) already exists")
            }
            if (try? deviceName(id)) == name {
                throw ErrorPayload(code: "duplicate_name", message: "a device named \(name) already exists")
            }
        }
        guard !resources.contains(where: { resource in
            switch resource.kind {
            case let .aggregate(existingUID, existingName, _, _):
                existingUID == uid || existingName == name
            case let .tap(_, aggregateUID):
                aggregateUID == uid
            }
        }) else {
            throw ErrorPayload(code: "duplicate_uid", message: "name or UID was already owned by this helper")
        }
        return (name, uid)
    }

    private func resourceRecord(_ aggregate: OwnedResource, tap: OwnedResource?) throws -> ResourceRecord {
        guard case let .aggregate(uid, name, physicalDeviceUID, direction) = aggregate.kind else {
            throw ErrorPayload(code: "internal_error", message: "resource is not an aggregate")
        }
        var tapID: UInt32?
        var tapUID: String?
        if let tap, case let .tap(uuid, _) = tap.kind {
            tapID = tap.id
            tapUID = uuid
        }
        return ResourceRecord(
            kind: tap == nil ? "subdevice_aggregate" : "tap_aggregate",
            id: aggregate.id,
            uid: uid,
            name: name,
            tapID: tapID,
            tapUID: tapUID,
            physicalDeviceUID: physicalDeviceUID,
            direction: direction,
            nominalRate: try? nominalRate(aggregate.id)
        )
    }

    private func defaultDeviceID(_ direction: Direction) throws -> AudioObjectID {
        try currentDefaultDeviceID(direction)
    }

    private func resolveCurrentDevice(uid: String) throws -> AudioObjectID {
        let matches = try allDeviceIDs().filter { (try? deviceUID($0)) == uid }
        guard matches.count == 1, let id = matches.first else {
            if matches.isEmpty {
                throw ErrorPayload(code: "device_not_found", message: "no current device has UID \(uid)")
            }
            throw ErrorPayload(code: "ambiguous_device", message: "multiple current devices have UID \(uid)")
        }
        return id
    }

    private func translateProcessID(_ processID: Int32) throws -> AudioObjectID {
        guard processID > 0 else {
            throw ErrorPayload(code: "invalid_process", message: "process_pid must be positive")
        }
        var address = propertyAddress(kAudioHardwarePropertyTranslatePIDToProcessObject)
        var processID = processID
        var objectID = AudioObjectID(kAudioObjectUnknown)
        var outputSize = UInt32(MemoryLayout<AudioObjectID>.size)
        let status = withUnsafePointer(to: &processID) { processPointer in
            withUnsafeMutablePointer(to: &objectID) { objectPointer in
                AudioObjectGetPropertyData(
                    AudioObjectID(kAudioObjectSystemObject),
                    &address,
                    UInt32(MemoryLayout<pid_t>.size),
                    processPointer,
                    &outputSize,
                    objectPointer
                )
            }
        }
        guard status == noErr else {
            throw coreAudioError("translate process PID", status)
        }
        guard objectID != kAudioObjectUnknown else {
            throw ErrorPayload(
                code: "process_not_audio_client",
                message: "process_pid does not identify a current CoreAudio client"
            )
        }
        return objectID
    }

    private func activeAggregate(uid: String) -> OwnedResource? {
        resources.reversed().first { resource in
            guard resource.active, case let .aggregate(resourceUID, _, _, _) = resource.kind else {
                return false
            }
            return resourceUID == uid
        }
    }

    private func validateRate(_ rate: Double) throws {
        guard rate.isFinite, rate > 0 else {
            throw ErrorPayload(code: "invalid_rate", message: "rate must be a positive finite number")
        }
    }

    private func rememberRate(uid: String, id: AudioObjectID) throws {
        guard !touchedRateUIDs.contains(uid) else {
            return
        }
        let original: Double
        if let startupRate = startupRates[uid] {
            original = startupRate
        } else {
            original = try nominalRate(id)
        }
        touchedRateUIDs.insert(uid)
        touchedRates.append(OriginalRate(uid: uid, idAtTouch: id, rate: original))
    }

    private func setNominalRate(
        id: AudioObjectID,
        uid: String,
        rate: Double,
        physicalDeviceUID: String?
    ) throws {
        let ranges = (try? availableRates(id)) ?? []
        if !ranges.isEmpty,
           !ranges.contains(where: { rate >= $0.mMinimum - 0.01 && rate <= $0.mMaximum + 0.01 })
        {
            throw ErrorPayload(code: "rate_unavailable", message: "device does not advertise rate \(rate)")
        }
        let current = try nominalRate(id)
        if abs(current - rate) < 0.01 {
            return
        }
        try rememberRate(uid: uid, id: id)
        if let physicalDeviceUID, let physicalID = try? resolveCurrentDevice(uid: physicalDeviceUID) {
            try rememberRate(uid: physicalDeviceUID, id: physicalID)
            lastSetRates[physicalDeviceUID] = rate
        }
        lastSetRates[uid] = rate
        try setScalarProperty(
            objectID: id,
            selector: kAudioDevicePropertyNominalSampleRate,
            value: rate,
            operation: "set nominal sample rate"
        )
        try waitForRate(id: id, expectedRate: rate)
        if let physicalDeviceUID,
           let physicalID = try? resolveCurrentDevice(uid: physicalDeviceUID),
           let physicalRate = try? nominalRate(physicalID)
        {
            lastSetRates[physicalDeviceUID] = physicalRate
        }
    }

    private func waitForDevice(uid: String, expectedID: AudioObjectID) throws {
        let deadline = Date().addingTimeInterval(2)
        repeat {
            if let id = try? resolveCurrentDevice(uid: uid), id == expectedID {
                return
            }
            usleep(20_000)
        } while Date() < deadline
        throw ErrorPayload(code: "device_not_published", message: "created device was not published within 2 seconds")
    }

    private func waitForDefault(_ direction: Direction, expectedID: AudioObjectID) throws {
        let deadline = Date().addingTimeInterval(2)
        repeat {
            if (try? defaultDeviceID(direction)) == expectedID {
                return
            }
            usleep(20_000)
        } while Date() < deadline
        throw ErrorPayload(
            code: "default_not_applied",
            message: "default \(direction.rawValue) device did not settle on the requested device within 2 seconds"
        )
    }

    private func waitForRate(id: AudioObjectID, expectedRate: Double) throws {
        let deadline = Date().addingTimeInterval(2)
        repeat {
            if let actual = try? nominalRate(id), abs(actual - expectedRate) < 0.01 {
                return
            }
            usleep(20_000)
        } while Date() < deadline
        let actual = try? nominalRate(id)
        throw ErrorPayload(
            code: "rate_not_applied",
            message: "device nominal rate did not settle at \(expectedRate) within 2 seconds (actual: \(String(describing: actual)))"
        )
    }

    private func explicitCleanup() throws {
        let errors = performCleanup()
        guard errors.isEmpty else {
            let first = errors[0]
            throw ErrorPayload(
                code: "cleanup_failed",
                message: errors.map(\.message).joined(separator: "; "),
                status: first.osStatus
            )
        }
    }

    private func performCleanup() -> [ErrorPayload] {
        guard !cleanupComplete else {
            return []
        }
        var errors: [ErrorPayload] = []

        for deviceID in Array(hoggedDeviceIDs) {
            do {
                try releaseHog(deviceID: deviceID)
            } catch let error as ErrorPayload {
                errors.append(error)
            } catch {
                errors.append(ErrorPayload(code: "internal_error", message: String(describing: error)))
            }
        }

        for direction in [Direction.input, .output] where touchedDefaults.contains(direction) {
            guard let uid = originalDefaults[direction] else {
                continue
            }
            do {
                let currentID = try defaultDeviceID(direction)
                let currentUID = try deviceUID(currentID)
                guard currentUID == lastSetDefaults[direction] else {
                    continue
                }
                let id = try resolveCurrentDevice(uid: uid)
                if currentID == id {
                    continue
                }
                try setScalarProperty(
                    objectID: AudioObjectID(kAudioObjectSystemObject),
                    selector: direction.defaultSelector,
                    value: id,
                    operation: "restore default \(direction.rawValue) device"
                )
                try waitForDefault(direction, expectedID: id)
            } catch let error as ErrorPayload {
                errors.append(error)
            } catch {
                errors.append(ErrorPayload(code: "internal_error", message: String(describing: error)))
            }
        }

        for original in touchedRates.reversed() {
            let ownedAggregates = resources.filter { resource in
                guard case let .aggregate(uid, _, _, _) = resource.kind else {
                    return false
                }
                return uid == original.uid
            }
            if !ownedAggregates.isEmpty, ownedAggregates.allSatisfy({ !$0.active }) {
                continue
            }
            if ownedAggregates.contains(where: { resource in
                resource.active
                    && (try? channelCount(resource.id, direction: .input)) == 0
                    && (try? channelCount(resource.id, direction: .output)) == 0
            }) {
                // The physical subdevice has its own saved rate entry. A
                // deliberately broken aggregate no longer has a clock whose
                // nominal rate can be restored before destruction.
                continue
            }
            do {
                let id: AudioObjectID
                if let current = try? resolveCurrentDevice(uid: original.uid) {
                    id = current
                } else if (try? deviceUID(original.idAtTouch)) == original.uid {
                    id = original.idAtTouch
                } else {
                    throw ErrorPayload(code: "device_not_found", message: "cannot restore rate for UID \(original.uid)")
                }
                if abs((try nominalRate(id)) - original.rate) < 0.01 {
                    continue
                }
                guard let lastSetRate = lastSetRates[original.uid],
                      abs((try nominalRate(id)) - lastSetRate) < 0.01
                else {
                    continue
                }
                try setScalarProperty(
                    objectID: id,
                    selector: kAudioDevicePropertyNominalSampleRate,
                    value: original.rate,
                    operation: "restore nominal sample rate for \(original.uid)"
                )
                try waitForRate(id: id, expectedRate: original.rate)
            } catch let error as ErrorPayload {
                errors.append(error)
            } catch {
                errors.append(ErrorPayload(code: "internal_error", message: String(describing: error)))
            }
        }

        for resource in resources.reversed() where resource.active {
            if let error = destroyOwnedResource(resource) {
                errors.append(error)
            }
        }
        cleanupComplete = errors.isEmpty
        return errors
    }

    private func destroyOwnedResource(_ resource: OwnedResource) -> ErrorPayload? {
        guard resource.active else {
            return nil
        }
        do {
            let status: OSStatus
            switch resource.kind {
            case let .aggregate(expectedUID, _, _, _):
                let actualUID = try deviceUID(resource.id)
                guard actualUID == expectedUID else {
                    return ErrorPayload(
                        code: "ownership_mismatch",
                        message: "refusing to destroy aggregate ID \(resource.id): expected UID \(expectedUID), found \(actualUID)"
                    )
                }
                status = AudioHardwareDestroyAggregateDevice(resource.id)
            case let .tap(expectedUUID, _):
                let actualUUID = try stringProperty(
                    objectID: resource.id,
                    selector: kAudioTapPropertyUID,
                    operation: "verify tap UID"
                )
                guard actualUUID.caseInsensitiveCompare(expectedUUID) == .orderedSame else {
                    return ErrorPayload(
                        code: "ownership_mismatch",
                        message: "refusing to destroy tap ID \(resource.id): expected UID \(expectedUUID), found \(actualUUID)"
                    )
                }
                status = AudioHardwareDestroyProcessTap(resource.id)
            }
            guard status == noErr else {
                return coreAudioError("destroy owned audio resource", status)
            }
            let deadline = Date().addingTimeInterval(2)
            switch resource.kind {
            case let .aggregate(expectedUID, _, _, _):
                while Date() < deadline {
                    if (try? resolveCurrentDevice(uid: expectedUID)) == nil {
                        break
                    }
                    usleep(20_000)
                }
                if (try? resolveCurrentDevice(uid: expectedUID)) != nil {
                    return ErrorPayload(
                        code: "resource_not_destroyed",
                        message: "aggregate \(expectedUID) remained published after destroy"
                    )
                }
            case .tap:
                while Date() < deadline {
                    if (try? stringProperty(
                        objectID: resource.id,
                        selector: kAudioTapPropertyUID,
                        operation: "verify destroyed tap"
                    )) == nil {
                        break
                    }
                    usleep(20_000)
                }
                if (try? stringProperty(
                    objectID: resource.id,
                    selector: kAudioTapPropertyUID,
                    operation: "verify destroyed tap"
                )) != nil {
                    return ErrorPayload(
                        code: "resource_not_destroyed",
                        message: "tap \(resource.id) remained published after destroy"
                    )
                }
            }
            resource.active = false
            return nil
        } catch let error as ErrorPayload {
            if error.osStatus == kAudioHardwareBadObjectError {
                resource.active = false
                return nil
            }
            return error
        } catch {
            return ErrorPayload(code: "internal_error", message: String(describing: error))
        }
    }
}

private final class JSONOutput: @unchecked Sendable {
    private let encoder: JSONEncoder

    init() {
        encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        encoder.keyEncodingStrategy = .convertToSnakeCase
    }

    func send(_ response: Response) {
        do {
            var data = try encoder.encode(response)
            data.append(0x0a)
            FileHandle.standardOutput.write(data)
        } catch {
            let fallback = "{\"error\":{\"code\":\"encoding_error\",\"message\":\"failed to encode response\"},\"ok\":false,\"type\":\"response\"}\n"
            FileHandle.standardOutput.write(Data(fallback.utf8))
        }
    }
}

private func reportCleanupErrors(_ errors: [String], context: String) {
    guard !errors.isEmpty else {
        return
    }
    let message = "audiodev \(context) cleanup failed: \(errors.joined(separator: "; "))\n"
    FileHandle.standardError.write(Data(message.utf8))
}

@available(macOS 14.6, *)
private func runHelper() {
    let queue = DispatchQueue(label: "com.audetic.repro.audiodev")
    let manager = AudioDeviceManager()
    let output = JSONOutput()
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase

    let signals = [SIGINT, SIGTERM, SIGHUP]
    let signalSources: [DispatchSourceSignal] = signals.map { number in
        signal(number, SIG_IGN)
        let source = DispatchSource.makeSignalSource(signal: number, queue: queue)
        source.setEventHandler {
            reportCleanupErrors(manager.cleanupErrors(), context: "signal \(number)")
            exit(128 + number)
        }
        source.resume()
        return source
    }
    withExtendedLifetime(signalSources) {
        var shouldStop = false
        while !shouldStop, let line = readLine(strippingNewline: true) {
            queue.sync {
                do {
                    _ = try JSONSerialization.jsonObject(with: Data(line.utf8))
                    let request = try decoder.decode(Request.self, from: Data(line.utf8))
                    let (response, stop) = manager.handle(request)
                    output.send(response)
                    shouldStop = stop
                } catch is DecodingError {
                    output.send(.failure(
                        id: nil,
                        op: nil,
                        error: ErrorPayload(code: "invalid_request", message: "request must match the JSON-lines command schema")
                    ))
                } catch {
                    output.send(.failure(
                        id: nil,
                        op: nil,
                        error: ErrorPayload(code: "invalid_json", message: "request must match the JSON-lines command schema")
                    ))
                }
            }
        }
        if !shouldStop {
            queue.sync {
                reportCleanupErrors(manager.cleanupErrors(), context: "stdin EOF")
            }
        }
    }
}

if #available(macOS 14.6, *) {
    runHelper()
} else {
    JSONOutput().send(.failure(
        id: nil,
        op: nil,
        error: ErrorPayload(code: "unsupported_os", message: "audiodev requires macOS 14.6 or newer")
    ))
}
