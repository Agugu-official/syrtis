import Foundation
import TokenBarCore

/// The single coordinator that builds one `tb_set_extra_scan_paths` payload
/// from every client's configured roots. The setter is full-replace, so a
/// payload for one client alone would silently clear the others; Claude's
/// expanded roots and the additional Pi roots therefore always travel
/// together.
///
/// The engine's `pi` lane scans `*.jsonl` under each root with the same parser
/// it uses for `~/.pi/agent/sessions`. A usage-only export (session header plus
/// assistant records with usage/model) and a full Pi session are both accepted
/// there; no second parser exists. Roots merge into the single Pi total, and
/// the Pi lane's cross-file `responseId` dedup means an export and a real
/// session for the same run count once.
enum ExtraScanRoots {
    /// Additional Pi directories, persisted exactly as configured.
    ///
    /// No default and no discovery: an empty list means Syrtis scans only the
    /// normal `~/.pi/agent/sessions` tree. These are user-chosen roots, not a
    /// producer-specific convention.
    enum Pi {
        static let storageKey = "tokenbar.pi.extraRoots"

        static func load() -> [String] {
            guard let raw = UserDefaults.standard.string(forKey: storageKey),
                let data = raw.data(using: .utf8),
                let paths = try? JSONDecoder().decode([String].self, from: data)
            else { return [] }
            return paths
        }

        static func save(_ paths: [String]) {
            let data = (try? JSONEncoder().encode(standardizedUnique(paths))) ?? Data("[]".utf8)
            UserDefaults.standard.set(
                String(data: data, encoding: .utf8) ?? "[]", forKey: storageKey)
        }

        /// Kept in the list when absent: an unmounted volume or a directory a
        /// runner has not created yet can become readable later, and the scan
        /// registry already retries it. Settings shows the warning; dropping
        /// the row would lose the user's configuration instead.
        static func isMissing(_ path: String) -> Bool {
            ClaudeExtraRoots.isMissing(path)
        }

        /// Home and the filesystem root are scan-everything roots; anything
        /// that exists and is not a directory can never be a session root and
        /// would otherwise be handed to `WalkDir`, which yields a regular file
        /// as its own entry. A missing path is not rejected here — it is
        /// reported as missing and kept for a later scan.
        static func isRejectedRoot(_ path: String) -> Bool {
            let standardized = standardizedPath(path)
            if ClaudeExtraRoots.isRejectedRoot(standardized) { return true }
            var isDirectory: ObjCBool = false
            let exists = FileManager.default.fileExists(
                atPath: standardized, isDirectory: &isDirectory)
            return exists && !isDirectory.boolValue
        }

        /// Probe paths off the main actor through the same serial queue the
        /// setter uses, so a probe issued after a save observes the filesystem
        /// that save was applied against rather than racing it.
        static func missingRoots(
            in paths: [String],
            then completion: @escaping @Sendable @MainActor (Set<String>) -> Void
        ) {
            ClaudeExtraRoots.missingRoots(in: paths) { missing in
                completion(missing)
            }
        }
    }

    /// The complete payload for both clients.
    ///
    /// With no Pi roots this returns `ClaudeExtraRoots.payloadJSON` unchanged
    /// byte-for-byte. That is not cosmetic: `appliedKey` stores the last
    /// accepted payload, and `install` drops the scan caches and advances the
    /// generation only when it moved. Re-encoding the same registry
    /// differently on the first launch after an upgrade would force a
    /// cache-bypassing rescan and reject the restart snapshot for a root set
    /// that never changed.
    ///
    /// The two-key form is assembled from two separately encoded arrays rather
    /// than a dictionary so its byte order is stable across launches. The
    /// engine reads a full object, and both keys are always present together:
    /// omitting one would clear that client's roots.
    static func payloadJSON(claudeConfigDirs: [String], piRoots: [String]) -> String {
        let piPaths = standardizedUnique(piRoots)
        guard !piPaths.isEmpty else {
            return ClaudeExtraRoots.payloadJSON(claudeConfigDirs)
        }
        let claudePaths = ClaudeExtraRoots.expandedPaths(claudeConfigDirs)
        return "{\"claude\":\(encode(claudePaths)),\"pi\":\(encode(piPaths))}"
    }

    /// Apply the merged registry at launch and after every edit. The install
    /// machinery (quota registry, applied marker, generation, cache
    /// invalidation, and quota wake) stays in `ClaudeExtraRoots`, which is
    /// where both clients' applies have always run.
    static func apply(
        then completion: (@Sendable @MainActor (ExtraScanPathsResult?) -> Void)? = nil
    ) {
        ClaudeExtraRoots.apply(
            scanPayload: {
                payloadJSON(
                    claudeConfigDirs: ClaudeExtraRoots.load(),
                    piRoots: Pi.load())
            },
            then: completion)
    }

    private static func standardizedPath(_ path: String) -> String {
        (path as NSString).standardizingPath
    }

    private static func standardizedUnique(_ paths: [String]) -> [String] {
        var seen = Set<String>()
        return paths.map(standardizedPath).filter { seen.insert($0).inserted }
    }

    private static func encode(_ paths: [String]) -> String {
        let data = (try? JSONEncoder().encode(paths)) ?? Data("[]".utf8)
        return String(data: data, encoding: .utf8) ?? "[]"
    }
}
