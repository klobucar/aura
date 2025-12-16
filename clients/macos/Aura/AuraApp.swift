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
        .defaultSize(width: 900, height: 600)
        .commands {
            CommandGroup(replacing: .newItem) { }
        }
    }
}
