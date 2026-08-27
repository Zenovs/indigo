// tray-icon über das statusnotifier-protokoll (ksni, reines rust).
// läuft in einem eigenen thread; fehlt der statusnotifier-host, wird nur
// eine meldung ausgegeben und das widget läuft ohne tray weiter.

pub enum TrayMsg {
    ToggleVisible,
    Quit,
}

struct IndigoTray {
    notify: glib::Sender<TrayMsg>,
}

impl ksni::Tray for IndigoTray {
    fn id(&self) -> String {
        "indigo".into()
    }

    fn title(&self) -> String {
        "indigo".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        match decode_icon(include_bytes!("../assets/icon-32.png")) {
            Some(icon) => vec![icon],
            None => Vec::new(),
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: "ein-/ausblenden".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.notify.send(TrayMsg::ToggleVisible);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "beenden".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.notify.send(TrayMsg::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

// dekodiert das eingebettete png (rgba8) und ordnet die pixel nach argb32
// in netzwerk-byte-reihenfolge um, wie es die statusnotifier-spec verlangt.
fn decode_icon(bytes: &[u8]) -> Option<ksni::Icon> {
    let decoder = png::Decoder::new(bytes);
    let mut reader = match decoder.read_info() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("tray: icon-png nicht lesbar: {e}");
            return None;
        }
    };
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = match reader.next_frame(&mut buf) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("tray: icon-png nicht dekodierbar: {e}");
            return None;
        }
    };
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        eprintln!("tray: icon-png ist nicht rgba8");
        return None;
    }
    let mut data = Vec::with_capacity(info.buffer_size());
    for px in buf[..info.buffer_size()].chunks_exact(4) {
        // rgba -> argb (je pixel: a, r, g, b)
        data.push(px[3]);
        data.push(px[0]);
        data.push(px[1]);
        data.push(px[2]);
    }
    Some(ksni::Icon {
        width: info.width as i32,
        height: info.height as i32,
        data,
    })
}

// startet den ksni-service in einem eigenen thread. bewusst run() statt
// TrayService::spawn(): spawn() startet zwar selbst einen thread, panict
// darin aber bei d-bus-fehlern — hier stattdessen stille fehlermeldung.
pub fn spawn(notify: glib::Sender<TrayMsg>) {
    std::thread::spawn(move || {
        let service = ksni::TrayService::new(IndigoTray { notify });
        if let Err(e) = service.run() {
            eprintln!("tray: statusnotifier-host nicht erreichbar: {e}");
        }
    });
}
