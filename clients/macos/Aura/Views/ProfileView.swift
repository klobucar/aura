import SwiftUI
import UniformTypeIdentifiers

/// Ultra-premium Liquid Glass view for editing the user profile.
struct ProfileView: View {
    @Environment(\.dismiss) var dismiss
    let client: QuicNetworkClient
    
    @State private var bio: String = ""
    @State private var avatarData: Data = Data()
    @State private var isAnimating = false
    
    init(client: QuicNetworkClient) {
        self.client = client
        let sessionId = client.sessionId ?? 0
        if let myProfile = client.profiles[sessionId] {
            _bio = State(initialValue: myProfile.bio)
            _avatarData = State(initialValue: Data(myProfile.avatarData))
        }
    }
    
    var body: some View {
        VStack(spacing: 0) {
            // Header
            HStack {
                Text("Edit Profile")
                    .font(.system(size: 20, weight: .bold))
                Spacer()
                Button(action: { dismiss() }) {
                    Image(systemName: "xmark")
                        .font(.system(size: 14, weight: .bold))
                        .foregroundStyle(.secondary)
                        .padding(8)
                        .background(Circle().fill(Color.white.opacity(0.1)))
                }
                .buttonStyle(.plain)
                .auraFluidHover()
            }
            .padding(24)
            
            ScrollView {
                VStack(spacing: 24) {
                    // Avatar Section
                    VStack(spacing: 16) {
                        ZStack {
                            // Animated aura rings
                            Circle()
                                .stroke(AuraTheme.Gradients.primary, lineWidth: 2)
                                .frame(width: 110, height: 110)
                                .opacity(isAnimating ? 0.3 : 0.6)
                                .scaleEffect(isAnimating ? 1.1 : 1.0)
                            
                            Circle()
                                .stroke(AuraTheme.Gradients.lushIndigo, lineWidth: 1)
                                .frame(width: 120, height: 120)
                                .opacity(isAnimating ? 0.1 : 0.4)
                                .scaleEffect(isAnimating ? 1.2 : 1.0)
                            
                            // Main avatar
                            avatarView
                                .frame(width: 100, height: 100)
                                .modifier(AuraTheme.Shadows.deep())
                            
                            // Camera trigger
                            VStack {
                                Spacer()
                                HStack {
                                    Spacer()
                                    Button(action: selectImage) {
                                        Image(systemName: "camera.fill")
                                            .font(.system(size: 12, weight: .bold))
                                            .foregroundStyle(.white)
                                            .padding(8)
                                            .background(Circle().fill(AuraTheme.Gradients.primary))
                                            .overlay(Circle().stroke(Color.white.opacity(0.2), lineWidth: 1))
                                    }
                                    .buttonStyle(.plain)
                                    .auraFluidHover()
                                }
                            }
                            .frame(width: 100, height: 100)
                        }
                        .onAppear {
                            withAnimation(.easeInOut(duration: 2.0).repeatForever(autoreverses: true)) {
                                isAnimating = true
                            }
                        }
                        
                        Text("Personalize your appearance")
                            .font(.system(size: 12))
                            .foregroundStyle(.secondary)
                    }
                    .padding(.top, 10)
                    
                    // Bio Section
                    VStack(alignment: .leading, spacing: 12) {
                        HStack {
                            Image(systemName: "text.quote")
                                .foregroundStyle(AuraTheme.Colors.primary)
                                .font(.system(size: 11, weight: .bold))
                            Text("BIO")
                                .font(.system(size: 10, weight: .bold))
                                .foregroundStyle(.secondary)
                                .kerning(1)
                        }
                        
                        TextEditor(text: $bio)
                            .font(.system(size: 14))
                            .scrollContentBackground(.hidden)
                            .frame(height: 100)
                            .padding(12)
                            .background(Color.black.opacity(0.15))
                            .clipShape(.rect(cornerRadius: 12))
                            .overlay(
                                RoundedRectangle(cornerRadius: 12)
                                    .strokeBorder(Color.white.opacity(0.1), lineWidth: 0.5)
                            )
                        
                        Text("Describe yourself in a few words.")
                            .font(.system(size: 10))
                            .foregroundStyle(.secondary)
                    }
                    .auraGlassSection()
                }
                .padding(.horizontal, 24)
            }
            
            // Footer Actions
            HStack(spacing: 16) {
                Button("Discard") { dismiss() }
                    .buttonStyle(.plain)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(.secondary)
                    .auraFluidHover()
                
                Spacer()
                
                Button(action: saveProfile) {
                    Text("Save Changes")
                        .font(.system(size: 13, weight: .bold))
                        .foregroundStyle(.white)
                        .padding(.vertical, 10)
                        .padding(.horizontal, 24)
                        .background(AuraTheme.Gradients.lushIndigo)
                        .clipShape(Capsule())
                        .modifier(AuraTheme.Shadows.soft())
                }
                .buttonStyle(.plain)
                .auraFluidHover()
            }
            .padding(24)
            .background(VisualEffectBlur(auraMaterial: .header, blendingMode: .withinWindow))
        }
        .frame(width: 400, height: 550)
        .auraGlass(material: .hudWindow)
    }
    
    @ViewBuilder
    private var avatarView: some View {
        if let image = NSImage(data: avatarData) {
            Image(nsImage: image)
                .resizable()
                .aspectRatio(contentMode: .fill)
                .clipShape(Circle())
        } else {
            Circle()
                .fill(AuraTheme.Gradients.primary)
                .overlay(
                    Text(client.profiles[client.sessionId ?? 0]?.displayName.prefix(1).uppercased() ?? "?")
                        .font(.system(size: 40, weight: .bold))
                        .foregroundStyle(.white)
                )
        }
    }
    
    private func selectImage() {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        panel.allowedContentTypes = [.image]
        
        if panel.runModal() == .OK {
            if let url = panel.url, let data = try? Data(contentsOf: url) {
                self.avatarData = data
            }
        }
    }
    
    private func saveProfile() {
        Task {
            await client.updateProfile(bio: bio, avatarData: avatarData)
            dismiss()
        }
    }
}
