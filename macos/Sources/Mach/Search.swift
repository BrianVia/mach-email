import SwiftUI

struct SearchState {
    /// Passed to the engine as typed, so Gmail operators like `from:` work.
    var query = ""
    var results: [ThreadSummary] = []
    var selected = 0
    /// The query `results` answer; differs from `query` while a search is in flight.
    var answered = ""

    var selectedThread: ThreadSummary? { results.indices.contains(selected) ? results[selected] : nil }
}

extension AppModel {
    func openSearch() {
        search = SearchState()
        route = .search
    }

    func runSearch(_ query: String) async {
        let trimmed = query.trimmingCharacters(in: .whitespaces)
        do {
            let results = trimmed.isEmpty ? [] : try await engine.search(trimmed, limit: 50)
            guard search?.query == query else { return }
            search?.results = results
            search?.selected = 0
            search?.answered = query
        } catch {
            show(error)
        }
    }

    /// The field keeps typing; Esc, arrows, and Return drive the results.
    func handleSearchKey(_ chord: String) -> Bool {
        guard let state = search else { return false }
        switch chord {
        case "esc":
            route = .list
        case "down", "up":
            search?.selected = min(max(state.selected + (chord == "down" ? 1 : -1), 0), max(state.results.count - 1, 0))
        case "enter":
            if let thread = state.selectedThread {
                Task { do { try await openThread(thread.id) } catch { show(error) } }
            }
        default:
            return false
        }
        return true
    }
}

/// Replaces the thread list while a search is open, and stays beside a
/// thread opened from it so Esc comes back to the same results.
struct SearchView: View {
    @Bindable var model: AppModel
    let state: SearchState
    @FocusState private var fieldFocused: Bool

    var body: some View {
        let multipleAccounts = Set(state.results.map(\.accountId)).count > 1
        ScrollViewReader { proxy in
            List(selection: selection) {
                ForEach(state.results) { thread in
                    ThreadRow(thread: thread, accountLabel: multipleAccounts ? model.accountLabel(thread.accountId) : nil)
                        .tag(thread.id)
                        .id(thread.id)
                        .contentShape(Rectangle())
                        .simultaneousGesture(TapGesture().onEnded { open(thread.id) })
                }
            }
            .contextMenu(forSelectionType: String.self, menu: { _ in }) { ids in
                if let id = ids.first { open(id) }
            }
            .overlay { emptyState }
            .safeAreaInset(edge: .top, spacing: 0) { field }
            .onChange(of: state.selectedThread?.id) { _, id in
                if let id { proxy.scrollTo(id) }
            }
        }
        .task(id: state.query) {
            try? await Task.sleep(for: .milliseconds(120))
            guard !Task.isCancelled else { return }
            await model.runSearch(state.query)
        }
        .onAppear { fieldFocused = model.route.name == "search" }
        .onChange(of: model.route.name) { _, name in fieldFocused = name == "search" }
    }

    private var field: some View {
        HStack(spacing: 6) {
            Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
            TextField("Search", text: query, prompt: Text("from:amazon, has:attachment, subject…"))
                .textFieldStyle(.plain)
                .focused($fieldFocused)
            if !state.query.isEmpty {
                Text(state.results.count.formatted())
                    .font(.caption)
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
                Button("Clear Search", systemImage: "xmark.circle.fill") { model.search?.query = "" }
                    .labelStyle(.iconOnly)
                    .buttonStyle(.plain)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .background(.quaternary.opacity(0.6), in: RoundedRectangle(cornerRadius: 7))
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.bar)
    }

    @ViewBuilder private var emptyState: some View {
        let trimmed = state.query.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty {
            ContentUnavailableView("Search Mail", systemImage: "magnifyingglass",
                description: Text("Search subjects, senders, and bodies. Gmail operators like from: and has:attachment work."))
        } else if state.results.isEmpty && state.answered == state.query {
            ContentUnavailableView.search(text: trimmed)
        }
    }

    private var query: Binding<String> {
        Binding { model.search?.query ?? "" } set: { model.search?.query = $0 }
    }

    private var selection: Binding<String?> {
        Binding {
            state.selectedThread?.id
        } set: { id in
            guard let index = state.results.firstIndex(where: { $0.id == id }) else { return }
            model.search?.selected = index
        }
    }

    private func open(_ id: String) {
        guard model.reader?.thread.id != id else { return }
        Task { do { try await model.openThread(id) } catch { model.show(error) } }
    }
}
