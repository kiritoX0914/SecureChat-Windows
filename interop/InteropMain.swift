// Live interop test: the real Swift ChatManager (from SecureChat-macOS) plays
// role S against two Rust peers (R1 = smaller id → X3DH initiator,
// R2 = larger id → responder). Mirrors role "S" in src/bin/sc_peer.rs.
import Foundation
import CryptoKit

let S  = "B0000000-0000-0000-0000-00000000000B"
let R1 = "A0000000-0000-0000-0000-00000000000A"
let R2 = "C0000000-0000-0000-0000-00000000000C"

@MainActor
final class Runner {
    let start = Date()
    var cm: ChatManager!
    var failures = 0
    var passes = 0

    func log(_ s: String) {
        print("[S \(String(format: "%6.2f", Date().timeIntervalSince(start)))] \(s)")
        fflush(stdout)
    }
    func check(_ name: String, _ ok: Bool) {
        if ok { passes += 1 } else { failures += 1 }
        log((ok ? "PASS " : "FAIL ") + name)
    }
    func at(_ t: Double, _ block: @escaping @MainActor () -> Void) {
        Task { @MainActor in
            let delay = t - Date().timeIntervalSince(self.start)
            if delay > 0 { try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000)) }
            block()
        }
    }
    func messages(_ conv: String) -> [ChatMessage] { cm.conversations[conv]?.messages ?? [] }
    func find(_ conv: String, _ text: String) -> ChatMessage? { messages(conv).first { $0.text == text } }
    func groupId() -> String? { cm.conversations.values.first { $0.isGroup }?.id }
    func incoming(_ conv: String, _ type: ChatMessage.ContentType) -> ChatMessage? {
        messages(conv).first { $0.contentType == type && $0.senderId == conv }
    }
    func fileInfo(_ m: ChatMessage?) -> (String, Int)? {
        guard let rel = m?.attachment?.localFile, let data = try? Data(contentsOf: AttachmentStore.location(rel)) else { return nil }
        return (SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined(), data.count)
    }

    func run() {
        let env = ProcessInfo.processInfo.environment
        let filePath = env["SC_TEST_FILE"] ?? ""
        let fileSha = env["SC_TEST_SHA"] ?? ""
        cm = ChatManager(deviceId: S, displayName: "User-S", persistence: PersistenceManager())
        log("started as \(S)")
        at(1) { self.log("listening port \(String(describing: self.cm.networkManager.listeningPort))") }

        at(8) {
            self.cm.sendMessage("hello from S", to: R1)
            self.cm.sendMessage("hello from S", to: R2)
        }
        at(10) { self.cm.sendFile(at: URL(fileURLWithPath: filePath), to: R2) }
        at(12) {
            self.check("S got text from R1", self.find(R1, "hello from R1") != nil)
            self.check("S got text from R2", self.find(R2, "hello from R2") != nil)
            self.check("S text delivered to R1", (self.find(R1, "hello from S")?.status ?? .sending) >= .delivered)
            if let m = self.find(R2, "hello from R2") { self.cm.sendMessage("S replies to R2", to: R2, replyTo: m) }
        }
        at(14) { self.cm.updateDisplayName("S Renamed") }
        at(16) { self.cm.markAsRead(peerId: R1) }
        at(18) {
            if let m = self.find(R2, "hello from S") { self.cm.deleteMessages([m.id], in: R2, forEveryone: true) }
        }
        at(24) {
            self.check("S: R1's deleted-for-both message is gone", self.find(R1, "hello from R1") == nil)
            let mine = self.find(R1, "hello from S")
            let rep = self.find(R1, "R1 replies")
            self.check("S got reply with replyToId", rep != nil && mine != nil && rep?.replyToId == mine?.id)
            let f = self.incoming(R1, .file)
            self.check("S file from R1 complete", f?.attachment?.isComplete == true)
            self.check("S file sha256 matches", self.fileInfo(f)?.0 == fileSha)
            let img = self.incoming(R1, .image)
            self.check("S got image from R1", (self.fileInfo(img)?.1 ?? 0) > 0 && img?.attachment?.mimeType == "image/jpeg")
            let p = self.cm.conversations[R1]?.peer
            self.check("S sees R1's new name", p?.displayName == "R1 Renamed")
            self.check("S sees R1's avatar", (p?.avatarData?.count ?? 0) > 100)
        }
        at(25) { self.cm.createGroup(name: "Interop Group", members: [R1, R2]) }
        at(29) { if let g = self.groupId() { self.cm.sendMessage("group hi from S", to: g) } }
        at(36) {
            let g = self.groupId() ?? ""
            self.check("S got group msg from R1", self.find(g, "group hi from R1") != nil)
            self.check("S got group msg from R2", self.find(g, "group hi from R2") != nil)
            self.check("S group msg delivered to all", self.find(g, "group hi from S")?.pendingRecipients == nil)
        }
        at(40) {
            let g = self.groupId() ?? ""
            self.check("S sees group renamed by R2", self.cm.conversations[g]?.group?.name == "Renamed Group")
        }
        at(41) { self.cm.clearChat(R1, forEveryone: true) }
        at(46) {
            for conv in self.cm.conversations.values {
                self.log("conv \(conv.id.prefix(8)) name=\(conv.peer.displayName) group=\(conv.group?.name ?? "-") msgs=\(conv.messages.count)")
                for m in conv.messages {
                    self.log("   msg from=\(m.senderId.prefix(1)) status=\(m.status) text=\(m.text.prefix(40)) pending=\(m.pendingRecipients?.map { String($0.prefix(1)) } ?? [])")
                }
            }
            self.log("RESULT \(self.failures == 0 ? "PASS" : "FAIL") passes=\(self.passes) failures=\(self.failures)")
            self.cm.persistence.flushNow()
            exit(self.failures == 0 ? 0 : 1)
        }
    }
}

MainActor.assumeIsolated {
    let runner = Runner()
    runner.run()
    _ = Unmanaged.passRetained(runner)
}
RunLoop.main.run()
