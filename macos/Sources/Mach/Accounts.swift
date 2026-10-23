import SwiftUI

extension AppModel {
    /// Signs in a new account, or signs an expired one in again when
    /// `loginHint` names it. The browser does the OAuth flow; the notice stays
    /// up until the engine returns.
    func addAccount(loginHint: String?) async {
        guard !system.signingIn else { return }
        system.signingIn = true
        defer { system.signingIn = false }
        show(nil)
        notice = Notice(text: "Finish signing in in your browser…")
        do {
            let email = try await engine.addAccount(loginHint: loginHint)
            status = try engine.accountStatus()
            labels = try await engine.listLabels()
            await refreshList()
            show("\(loginHint == nil ? "Added" : "Signed in again to") \(accountLabel(email))")
        } catch {
            show(error)
        }
    }
}

struct ReauthBanner: View {
    let model: AppModel
    let email: String

    var body: some View {
        SystemBanner(symbol: "person.crop.circle.badge.exclamationmark", text: "Sign-in expired for \(model.accountLabel(email)).") {
            Button("Sign In Again") { Task { await model.addAccount(loginHint: email) } }
                .disabled(model.system.signingIn)
        }
    }
}
