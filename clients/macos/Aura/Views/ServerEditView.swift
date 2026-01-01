import SwiftUI

struct ServerEditView: View {
    @Environment(\.dismiss) var dismiss
    @ObservedObject var serverManager: ServerManager
    
    let server: ServerProfile?
    
    @State private var name: String
    @State private var host: String
    @State private var port: String
    @State private var password: String
    @State private var isFavorite: Bool
    
    init(serverManager: ServerManager, server: ServerProfile? = nil) {
        self.serverManager = serverManager
        self.server = server
        _name = State(initialValue: server?.name ?? "")
        _host = State(initialValue: server?.host ?? "")
        _port = State(initialValue: String(server?.port ?? 8443))
        _password = State(initialValue: server?.password ?? "")
        _isFavorite = State(initialValue: server?.isFavorite ?? false)
    }
    
    var body: some View {
        VStack(spacing: 20) {
            Text(server == nil ? "Add Server" : "Edit Server")
                .font(.title2.bold())
            
            VStack(spacing: 16) {
                // Name
                VStack(alignment: .leading, spacing: 6) {
                    Text("Name")
                        .font(.caption.bold())
                        .foregroundColor(.secondary)
                    TextField("My Server", text: $name)
                        .textFieldStyle(.roundedBorder)
                }
                
                // Host
                VStack(alignment: .leading, spacing: 6) {
                    Text("Host")
                        .font(.caption.bold())
                        .foregroundColor(.secondary)
                    TextField("127.0.0.1", text: $host)
                        .textFieldStyle(.roundedBorder)
                }
                
                // Port
                VStack(alignment: .leading, spacing: 6) {
                    Text("Port")
                        .font(.caption.bold())
                        .foregroundColor(.secondary)
                    TextField("8443", text: $port)
                        .textFieldStyle(.roundedBorder)
                }
                
                // Password
                VStack(alignment: .leading, spacing: 6) {
                    Text("Server Password (Optional)")
                        .font(.caption.bold())
                        .foregroundColor(.secondary)
                    SecureField("", text: $password)
                        .textFieldStyle(.roundedBorder)
                }
                
                // Favorite
                Toggle("Favorite", isOn: $isFavorite)
            }
            .padding()
            .auraGlass()
            
            // Buttons
            HStack {
                Button("Cancel") { dismiss() }
                    .buttonStyle(.bordered)
                
                Spacer()
                
                Button("Save") {
                    saveServer()
                }
                .buttonStyle(.borderedProminent)
                .disabled(name.isEmpty || host.isEmpty)
            }
        }
        .padding(30)
        .frame(width: 400)
    }
    
    private func saveServer() {
        let portValue = UInt16(port) ?? 8443
        
        if let existing = server {
            var updated = existing
            updated.name = name
            updated.host = host
            updated.port = portValue
            updated.password = password.isEmpty ? nil : password
            updated.isFavorite = isFavorite
            serverManager.updateServer(updated)
        } else {
            let newServer = ServerProfile(
                name: name,
                host: host,
                port: portValue,
                password: password.isEmpty ? nil : password,
                isFavorite: isFavorite
            )
            serverManager.addServer(newServer)
        }
        
        dismiss()
    }
}
