extern crate conrod;
use conrod::backend::glium::glium::{self, Surface};
use conrod::backend::glium::glium::texture::srgb_texture2d::SrgbTexture2d;
use conrod::backend::glium::glium::texture::RawImage2d;
use conrod::backend::glium::glium::glutin::{Event, WindowEvent, VirtualKeyCode, ElementState};
use std;
extern crate crossbeam;
extern crate rusttype;

pub struct ChimperWindow {
  evloop: glium::glutin::EventsLoop,
  display: glium::Display,
  renderer: conrod::backend::glium::Renderer,
  image_map: conrod::image::Map<SrgbTexture2d>,
  initial_width: u32,
  initial_height: u32,
}

pub trait ChimperApp: Sync+Send {
  fn initialize(&mut self, _ui: &mut conrod::Ui) {}
  fn draw_gui(&mut self, ui: &mut conrod::Ui, evproxy: &glium::glutin::EventsLoopProxy) -> bool;
  fn process_event(&mut self, event: &conrod::event::Input);
}

impl ChimperWindow {
  pub fn new(name: &str, initial_width: u32, initial_height: u32) -> Self {
    // Build the window.
    let evloop = glium::glutin::EventsLoop::new();
    let window = glium::glutin::WindowBuilder::new()
      .with_title(name)
      .with_dimensions(initial_width, initial_height);
    let context = glium::glutin::ContextBuilder::new()
      .with_vsync(true)
      .with_multisampling(4);
    let display = glium::Display::new(window, context, &evloop).unwrap();

    // A type used for converting `conrod::render::Primitives` into `Command`s that can be used
    // for drawing to the glium `Surface`.
    let renderer = conrod::backend::glium::Renderer::new(&display).unwrap();

    let image_map = conrod::image::Map::new();

    Self {
      evloop,
      display,
      renderer,
      image_map,
      initial_width,
      initial_height,
    }
  }

  // The main conrod UI loop
  fn run_conrod(event_rx: std::sync::mpsc::Receiver<conrod::event::Input>,
                render_tx: std::sync::mpsc::Sender<conrod::render::OwnedPrimitives>,
                evproxy: glium::glutin::EventsLoopProxy,
                app: &mut ChimperApp,
                initial_width: u32, initial_height: u32) {
    // Construct our `Ui`.
    let mut ui = conrod::UiBuilder::new([initial_width as f64, initial_height as f64]).build();
    ui.fonts.insert(Self::load_font(include_bytes!("../fonts/NotoSans-Regular.ttf")));

    app.initialize(&mut ui);

    // Many widgets require another frame to finish drawing after clicks or hovers, so we
    // insert an update into the conrod loop using this `bool` after each event.
    let mut needs_update = true;
    'conrod: loop {
      // Collect any pending events.
      let mut events = Vec::new();
      while let Ok(event) = event_rx.try_recv() {
        events.push(event);
      }

      // If there are no events pending, wait for them.
      if events.is_empty() && !needs_update {
        match event_rx.recv() {
          Ok(event) => events.push(event),
          Err(_) => break 'conrod,
        };
      }

      needs_update = false;
      // Input each event into the `Ui`.
      for event in events {
        app.process_event(&event);
        ui.handle_event(event);
        needs_update = true;
      }

      needs_update = app.draw_gui(&mut ui, &evproxy) || needs_update;

      // Render the `Ui` to a list of primitives that we can send to the main thread for
      // display. Wakeup `winit` for rendering.
      if let Some(primitives) = ui.draw_if_changed() {
        if render_tx.send(primitives.owned()).is_err()
        || evproxy.wakeup().is_err() {
          break 'conrod;
        }
      }
    }
  }

  // Draws the given `primitives` to the given `Display`.
  fn draw(display: &glium::Display,
          renderer: &mut conrod::backend::glium::Renderer,
          image_map: &conrod::image::Map<SrgbTexture2d>,
          primitives: &conrod::render::OwnedPrimitives) {
    renderer.fill(display, primitives.walk(), &image_map);
    let mut target = display.draw();
    target.clear_color(0.0, 0.0, 0.0, 1.0);
    renderer.draw(display, &mut target, &image_map).unwrap();
    target.finish().unwrap();
  }


  pub fn run<F>(&mut self, app: &mut ChimperApp, closure: F)
    where F: Fn(&mut glium::Display,
                &mut conrod::backend::glium::Renderer,
                &mut conrod::image::Map<SrgbTexture2d>, 
                glium::glutin::EventsLoopProxy) -> bool {
    crossbeam::scope(|scope| {
      // A channel to send events from the main `winit` thread to the conrod thread.
      let (event_tx, event_rx) = std::sync::mpsc::channel();
      // A channel to send `render::Primitive`s from the conrod thread to the `winit thread.
      let (render_tx, render_rx) = std::sync::mpsc::channel();

      let evproxy = self.evloop.create_proxy();
      let w = self.initial_width;
      let h = self.initial_height;
      scope.spawn(move ||Self::run_conrod(event_rx, render_tx, evproxy, app, w, h));

      // Run the `winit` loop.
      let mut last_update = std::time::Instant::now();
      let mut closed = false;
      let mut fullscreen = false;
      while !closed {
        // We don't want to loop any faster than 60 FPS, so wait until it has been at least
        // 16ms since the last yield.
        let sixteen_ms = std::time::Duration::from_millis(16);
        let now = std::time::Instant::now();
        let duration_since_last_update = now.duration_since(last_update);
        if duration_since_last_update < sixteen_ms {
          std::thread::sleep(sixteen_ms - duration_since_last_update);
        }

        let evproxy = self.evloop.create_proxy();
        let evloop = &mut self.evloop;
        let display = &mut self.display;
        let renderer = &mut self.renderer;
        let image_map = &mut self.image_map;

        // Give up and use polling for now until there's a clean way to do it properly
        evloop.poll_events(|event| {
          // Use the `winit` backend feature to convert the winit event to a conrod one.
          if let Some(event) = conrod::backend::winit::convert_event(event.clone(), display) {
            event_tx.send(event).unwrap();
          }

          match event {
            Event::WindowEvent { event, .. } => match event {
              // Break from the loop upon `Escape`.
              WindowEvent::Closed |
              WindowEvent::KeyboardInput {
                input: glium::glutin::KeyboardInput {
                  virtual_keycode: Some(VirtualKeyCode::Escape),
                  ..
                },
                ..
              } => {
                closed = true;
              },
              WindowEvent::KeyboardInput {
                input: glium::glutin::KeyboardInput {
                  virtual_keycode: Some(VirtualKeyCode::F11),
                  state: ElementState::Pressed,
                  ..
                },
                ..
              } => {
                fullscreen = !fullscreen;
                if fullscreen {
                  let monitor = display.gl_window().window().get_current_monitor();
                  display.gl_window().window().set_fullscreen(Some(monitor));
                } else {
                  display.gl_window().window().set_fullscreen(None);
                }
              },
              _ => {},
            },
            _ => (),
          }
        });

        // Run any app specific code and then redraw in case things have changed
        if closure(display, renderer, image_map, evproxy) {
          event_tx.send(conrod::event::Input::Redraw).unwrap();
        }

        // Draw the most recently received `conrod::render::Primitives` sent from the `Ui`.
        if let Some(primitives) = render_rx.try_iter().last() {
            Self::draw(&display, renderer, &image_map, &primitives);
        }

        last_update = std::time::Instant::now();
      }

      // Make sure the conrod thread terminates so the app exits
      drop(event_tx);
    });
  }

  fn load_font(buf: &[u8]) -> rusttype::Font {
    rusttype::FontCollection::from_bytes(buf).unwrap().into_font().unwrap()
  }

  // Load the image from a file
  pub fn load_texture(&mut self, img: RawImage2d<u8>) -> conrod::image::Id {
    let texture = glium::texture::SrgbTexture2d::new(&self.display, img).unwrap();
    self.image_map.insert(texture)
  }
}
