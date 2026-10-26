import UIKit
import ObjectiveC

@_silgen_name("lightcraft_post_action")
func lightcraft_post_action(_ action: Int32)

@objc(SceneDelegate)
class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?
    private static var hasHooked = false

    override init() {
        super.init()
        NSLog("[LightCraft-Swift] SceneDelegate init")
        Self.enableWindowSceneAttachmentHook()
    }

    static func enableWindowSceneAttachmentHook() {
        guard !hasHooked else { return }
        hasHooked = true

        let origInit = #selector(UIWindow.init(frame:))
        let swizInit = #selector(UIWindow.lc_initWithFrame(_:))

        if let m1 = class_getInstanceMethod(UIWindow.self, origInit),
           let m2 = class_getInstanceMethod(UIWindow.self, swizInit) {
            method_exchangeImplementations(m1, m2)
            NSLog("[LightCraft-Swift] Swizzled UIWindow initWithFrame")
        }

        // winit's iOS `inner_size()` is the safe-area rectangle, and egui-wgpu sizes the Metal surface from it,
        // while the CAMetalLayer fills the whole window: the frame is drawn into a drawable that is too short and
        // stretched to the window. The app handles safe areas itself (`ios_get_safe_area`), so winit and egui see none.
        let getSel = #selector(getter: UIView.safeAreaInsets)
        let lcSel = #selector(UIWindow.lc_safeAreaInsets)
        if let mine = class_getInstanceMethod(UIWindow.self, lcSel),
           let own = class_getInstanceMethod(UIWindow.self, getSel) {
            // UIWindow's getter may be inherited from UIView: give UIWindow its own copy first, never touch UIView
            let inherited = class_getInstanceMethod(UIView.self, getSel).map { method_getImplementation($0) }
            if inherited != nil, method_getImplementation(own) == inherited,
               let enc = method_getTypeEncoding(own) {
                _ = class_addMethod(UIWindow.self, getSel, method_getImplementation(own), enc)
            }
            if let own2 = class_getInstanceMethod(UIWindow.self, getSel) {
                method_exchangeImplementations(own2, mine)
                NSLog("[LightCraft-Swift] Hooked UIWindow safeAreaInsets (own=%d)", inherited == nil ? 1 : 0)
            }
        }
    }

    func scene(
        _ scene: UIScene,
        willConnectTo session: UISceneSession,
        options connectionOptions: UIScene.ConnectionOptions
    ) {
        guard let windowScene = scene as? UIWindowScene else { return }
        NSLog("[LightCraft-Swift] Scene willConnectTo session")

        self.bindWindows(to: windowScene)

        DispatchQueue.main.async {
            NSLog("[LightCraft-Swift] Posting didBecomeActiveNotification after scene connection")
            NotificationCenter.default.post(
                name: UIApplication.didBecomeActiveNotification,
                object: UIApplication.shared
            )
        }

        NotificationCenter.default.addObserver(
            forName: UIWindow.didBecomeVisibleNotification,
            object: nil,
            queue: .main
        ) { [weak self, weak windowScene] notif in
            guard let ws = windowScene else { return }
            if let win = notif.object as? UIWindow {
                NSLog("[LightCraft-Swift] Window didBecomeVisible: %@", win)
                self?.bind(window: win, to: ws)
            }
        }
    }

    func sceneDidBecomeActive(_ scene: UIScene) {
        guard let windowScene = scene as? UIWindowScene else { return }
        NSLog("[LightCraft-Swift] sceneDidBecomeActive")
        bindWindows(to: windowScene)

        NotificationCenter.default.post(
            name: UIApplication.didBecomeActiveNotification,
            object: UIApplication.shared
        )
    }

    func sceneWillResignActive(_ scene: UIScene) {
        NSLog("[LightCraft-Swift] sceneWillResignActive")
    }

    func sceneWillEnterForeground(_ scene: UIScene) {
        guard let windowScene = scene as? UIWindowScene else { return }
        NSLog("[LightCraft-Swift] sceneWillEnterForeground")
        bindWindows(to: windowScene)
        // a USB drive plugged in while the app was away: bring its remembered folders back
        PhotoPickerManager.shared.restoreRememberedFolders()
    }

    func sceneDidEnterBackground(_ scene: UIScene) {
        NSLog("[LightCraft-Swift] sceneDidEnterBackground")
    }

    func windowScene(
        _ windowScene: UIWindowScene,
        didUpdate previousCoordinateSpace: UICoordinateSpace,
        interfaceOrientation previousInterfaceOrientation: UIInterfaceOrientation,
        traitCollection previousTraitCollection: UITraitCollection
    ) {
        NSLog("[LightCraft-Swift] windowScene didUpdate coordinateSpace: %@", NSCoder.string(for: windowScene.coordinateSpace.bounds))
        bindWindows(to: windowScene)
    }

    private func bind(window: UIWindow, to windowScene: UIWindowScene) {
        let className = String(describing: type(of: window))
        if className.contains("UITextEffects") || className.contains("SystemOverlay") || className.contains("Keyboard") {
            return
        }
        if window.windowScene !== windowScene {
            window.windowScene = windowScene
            NSLog("[LightCraft-Swift] Attached window %@ to windowScene %@", window, windowScene)
        }
        window.overrideUserInterfaceStyle = .dark
        window.rootViewController?.overrideUserInterfaceStyle = .dark
        window.frame = windowScene.coordinateSpace.bounds
        ios_update_safe_area()
        if self.window == nil {
            self.window = window
            window.makeKeyAndVisible()
        }

        // Attach Apple Pencil double-tap interaction (iPadOS 12.1+)
        if #available(iOS 12.1, *) {
            var hasPencil = false
            for interaction in window.interactions {
                if interaction is UIPencilInteraction {
                    hasPencil = true
                    break
                }
            }
            if !hasPencil {
                let pencil = UIPencilInteraction()
                pencil.delegate = self
                window.addInteraction(pencil)
                NSLog("[LightCraft-Swift] Attached UIPencilInteraction to window")
            }
            if !(window.gestureRecognizers ?? []).contains(where: { $0 is PencilStrokeRecognizer }) {
                window.addGestureRecognizer(PencilStrokeRecognizer(target: nil, action: nil))
                NSLog("[LightCraft-Swift] Attached Pencil stroke recognizer to window")
            }
        }

    }

    private func bindWindows(to windowScene: UIWindowScene) {
        for win in windowScene.windows {
            bind(window: win, to: windowScene)
        }
    }
}

/// True only while the host reads the real safe area; everyone else (winit, egui-winit) sees zero.
var lcRealSafeArea = false

func lcRealSafeAreaInsets(of window: UIWindow?) -> UIEdgeInsets? {
    lcRealSafeArea = true
    defer { lcRealSafeArea = false }
    return window?.safeAreaInsets
}

extension UIWindow {
    /// After the exchange, `lc_safeAreaInsets` is UIKit's own getter and `safeAreaInsets` is this body.
    @objc func lc_safeAreaInsets() -> UIEdgeInsets {
        let real = self.lc_safeAreaInsets()
        return lcRealSafeArea ? real : .zero
    }

    @objc func lc_initWithFrame(_ frame: CGRect) -> UIWindow {
        let win = self.lc_initWithFrame(frame)
        if let ws = UIApplication.shared.connectedScenes.compactMap({ $0 as? UIWindowScene }).first {
            win.windowScene = ws
        }
        return win
    }
}


extension SceneDelegate: UIPencilInteractionDelegate {
    @available(iOS 12.1, *)
    func pencilInteractionDidTap(_ interaction: UIPencilInteraction) {
        NSLog("[LightCraft-Swift] UIPencilInteraction double-tap -> posting ZoomToggle action (3)")
        lightcraft_post_action(3)
    }
}
