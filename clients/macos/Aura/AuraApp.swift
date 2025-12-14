//
//  AuraApp.swift
//  Aura
//

import SwiftUI

@main
struct AuraApp: App {
    var body: some Scene {
        WindowGroup {
            ContentView()
                .background(VisualEffectBlur(material: .headerView, blendingMode: .behindWindow))
        }
        .windowStyle(HiddenTitleBarWindowStyle())
    }
}
