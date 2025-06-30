#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// use tauri::Manager;

// fn main() {
//     let todo_api = todo_core::AppState::new();

//     tauri::Builder::default()
//         .manage(todo_api)
//         .setup(|app| {
//             // let window = app.get_webview_window("main").unwrap();
//             // window.set_decorations(false)?;
//             Ok(())
//         })
//         // .on_window_event(|event| {
//         //     if let tauri::WindowEvent:: { position, .. } = event.event() {
//         //         if position.y < 40.0 {
//         //             // 40px tall invisible drag region
//         //             event.window().start_dragging().ok();
//         //         }
//         //     }
//         // })
//         .run(tauri::generate_context!())
//         .expect("error while running tauri application");
// }

use tauri::{TitleBarStyle, WebviewUrl, WebviewWindowBuilder};

pub fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let win_builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
                .title("Transparent Titlebar Window")
                .inner_size(800.0, 600.0);

            // set transparent title bar only when building for macOS
            #[cfg(target_os = "macos")]
            let win_builder = win_builder.title_bar_style(TitleBarStyle::Transparent);

            let window = win_builder.build().unwrap();

            // set background color only when building for macOS
            #[cfg(target_os = "macos")]
            {
                use cocoa::appkit::{NSColor, NSWindow};
                use cocoa::base::{id, nil};

                let ns_window = window.ns_window().unwrap() as id;
                unsafe {
                    let bg_color = NSColor::colorWithRed_green_blue_alpha_(
                        nil,
                        0.0 / 255.0,
                        0.0 / 255.0,
                        0.0 / 255.0,
                        1.0,
                    );
                    ns_window.setBackgroundColor_(bg_color);
                }
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// use tauri::w::{
//     event::{ElementState, Event, MouseButton, WindowEvent},
//     event_loop::{ControlFlow, EventLoop},
//     window::WindowBuilder,
// };

// fn main() {
//     let event_loop = EventLoop::new();
//     let window = WindowBuilder::new()
//         .with_title("Borderless Draggable Window")
//         .with_decorations(false) // This removes the title bar and borders
//         .build(&event_loop)
//         .unwrap();

//     let mut drag_start: Option<(f64, f64)> = None;

//     event_loop.run(move |event, _, control_flow| {
//         *control_flow = ControlFlow::Wait;

//         match event {
//             Event::WindowEvent {
//                 event: WindowEvent::CloseRequested,
//                 ..
//             } => *control_flow = ControlFlow::Exit,

//             Event::WindowEvent {
//                 event:
//                     WindowEvent::MouseInput {
//                         state: ElementState::Pressed,
//                         button: MouseButton::Left,
//                         ..
//                     },
//                 ..
//             } => {
//                 if drag_start.is_none() {
//                     // Start drag
//                     if let Some(pos) = window.outer_position().ok() {
//                         drag_start = Some((pos.x as f64, pos.y as f64));
//                     }
//                 }
//             }

//             Event::WindowEvent {
//                 event: WindowEvent::CursorMoved { position, .. },
//                 ..
//             } => {
//                 if let Some(start) = drag_start {
//                     let new_x = start.0 + position.x - start.0;
//                     let new_y = start.1 + position.y - start.1;
//                     window.set_outer_position(tauri::w::dpi::PhysicalPosition::new(new_x, new_y));
//                 }
//             }

//             Event::WindowEvent {
//                 event:
//                     WindowEvent::MouseInput {
//                         state: ElementState::Released,
//                         button: MouseButton::Left,
//                         ..
//                     },
//                 ..
//             } => {
//                 drag_start = None;
//             }

//             _ => {}
//         }
//     });
// }
