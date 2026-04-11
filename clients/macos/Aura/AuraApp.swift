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
                .background {
                    if #available(macOS 26, *) {
                        Color.clear // System handles Liquid Glass chrome
                    } else {
                        VisualEffectBlur(material: .headerView, blendingMode: .behindWindow)
                    }
                }
        }
        .windowStyle(.hiddenTitleBar)
        .defaultSize(width: 960, height: 640)
        .commands {
            CommandGroup(replacing: .newItem) { }
        }
    }
}
