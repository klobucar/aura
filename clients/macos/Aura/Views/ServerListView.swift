import SwiftUI

struct ServerListView: View {
    @Environment(\.dismiss) var dismiss
    @StateObject private var serverManager = ServerManager()
    @State private var showingAddServer = false
    @State private var editingServer: ServerProfile?
    
    var onSelect: ((ServerProfile) -> Void)?
    
    var body: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                Text("Servers")
                    .font(.title2.bold())
                Spacer()
                Button(action: { showingAddServer = true }) {
                    Image(systemName: "plus.circle.fill")
                        .accessibilityLabel("Add")
                        .font(.title2)
                        .foregroundStyle(AuraTheme.Gradients.lushIndigo)
                }
                .buttonStyle(.plain)
            }
            .padding()
            
            Divider()
            
            // Server List
            ScrollView {
                LazyVStack(spacing: 12) {
                    // Favorites
                    if !serverManager.favoriteServers.isEmpty {
                        sectionHeader("Favorites")
                        ForEach(serverManager.favoriteServers) { server in
                            serverRow(server)
                        }
                    }
                    
                    // Recent
                    if !serverManager.recentServers.isEmpty {
                        sectionHeader("Recent")
                        ForEach(serverManager.recentServers) { server in
                            serverRow(server)
                        }
                    }
                    
                    // All Servers
                    sectionHeader("All Servers")
                    ForEach(serverManager.servers) { server in
                        serverRow(server)
                    }
                }
                .padding()
            }
        }
        .frame(width: 450, height: 500)
        .sheet(isPresented: $showingAddServer) {
            ServerEditView(serverManager: serverManager)
        }
        .sheet(item: $editingServer) { server in
            ServerEditView(serverManager: serverManager, server: server)
        }
    }
    
    private func sectionHeader(_ title: String) -> some View {
        HStack {
            Text(title)
                .font(.caption.bold())
                .foregroundStyle(.secondary)
                .textCase(.uppercase)
            Spacer()
        }
        .padding(.top, 8)
    }
    
    private func serverRow(_ server: ServerProfile) -> some View {
        HStack(spacing: 12) {
            // Icon
            Image(systemName: server.isFavorite ? "star.fill" : "server.rack")
                .font(.title3)
                .foregroundStyle(server.isFavorite ? .yellow : AuraTheme.Colors.primary)
                .frame(width: 32)
            
            // Info
            VStack(alignment: .leading, spacing: 4) {
                Text(server.name)
                    .font(.system(size: 14, weight: .semibold))
                Text("\\(server.host):\\(server.port)")
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
            }
            
            Spacer()
            
            // Select button
            if onSelect != nil {
                Button("Select") {
                    onSelect?(server)
                    dismiss()
                }
                .buttonStyle(.borderedProminent)
            }
        }
        .padding(12)
        .auraGlass(cornerRadius: 10)
        .contextMenu {
            Button(action: { editingServer = server }) {
                Label("Edit", systemImage: "pencil")
            }
            Button(action: { serverManager.deleteServer(id: server.id) }) {
                Label("Delete", systemImage: "trash")
            }
        }
    }
}
