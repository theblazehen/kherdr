use i_slint_core::{item_tree::ItemRc, items, window::WindowInner};
use serde::Serialize;
use std::collections::BTreeMap;
use base64::{Engine, engine::general_purpose::STANDARD};

#[derive(Serialize)]
pub struct Node {
    pub id: usize,
    pub parent: Option<usize>,
    pub name: String,
    pub kind: String,
    pub rect: [f32; 4],
    pub styles: BTreeMap<String, String>,
    pub text: Option<String>,
}
#[derive(Serialize)]
pub struct Scene {
    pub schema: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub nodes: Vec<Node>,
    pub unsupported: Vec<String>,
}
fn color(brush: slint::Brush) -> Result<String, String> {
    match brush {
        slint::Brush::SolidColor(c) => Ok(format!(
            "#{:02x}{:02x}{:02x}{:02x}",
            c.red(),
            c.green(),
            c.blue(),
            c.alpha()
        )),
        other => Err(format!("Unsupported brush: {other:?}")),
    }
}
fn font_styles(node: &mut Node, font: i_slint_core::graphics::FontRequest) {
    node.styles.insert(
        "font-family".into(),
        font.family.unwrap_or_default().to_string(),
    );
    node.styles.insert(
        "font-size".into(),
        format!("{}px", font.pixel_size.unwrap_or_default().get()),
    );
    node.styles
        .insert("font-weight".into(), font.weight.unwrap_or(400).to_string());
    node.styles.insert(
        "font-style".into(),
        if font.italic { "italic" } else { "normal" }.into(),
    );
    node.styles.insert(
        "letter-spacing".into(),
        format!("{}px", font.letter_spacing.unwrap_or_default().get()),
    );
}
fn text<T: i_slint_core::item_rendering::RenderText + i_slint_core::item_rendering::HasFont>(
    value: std::pin::Pin<&T>,
    item: &ItemRc,
    node: &mut Node,
) -> Result<(), String> {
    use i_slint_core::item_rendering::PlainOrStyledText;
    let font = value.font_request(item);
    node.kind = "text".into();
    node.text = Some(match value.text() {
        PlainOrStyledText::Plain(text) => text.to_string(),
        _ => return Err("Styled text is not supported".into()),
    });
    node.styles.insert("color".into(), color(value.color())?);
    font_styles(node, font);
    let (horizontal, vertical) = value.alignment();
    node.styles.insert(
        "text-align".into(),
        format!("{horizontal:?}").to_lowercase(),
    );
    node.styles.insert(
        "vertical-align".into(),
        format!("{vertical:?}").to_lowercase(),
    );
    node.styles.insert(
        "white-space".into(),
        if value.wrap() == items::TextWrap::NoWrap {
            "pre"
        } else {
            "pre-wrap"
        }
        .into(),
    );
    node.styles.insert(
        "text-overflow".into(),
        if value.overflow() == items::TextOverflow::Elide {
            "ellipsis"
        } else {
            "clip"
        }
        .into(),
    );
    Ok(())
}
fn image(value: std::pin::Pin<&items::ImageItem>, node: &mut Node) -> Result<(), String> {
    use i_slint_core::{ImageInner, graphics::{IntSize, SharedImageBuffer}};
    let source = value.source();
    let source: &ImageInner = (&source).into();
    if matches!(source, ImageInner::None) || node.rect[2] <= 0. || node.rect[3] <= 0. {
        node.kind = "group".into();
        return Ok(());
    }
    let size = IntSize::new(node.rect[2].ceil() as u32, node.rect[3].ceil() as u32);
    let buffer = source.render_to_buffer(Some(size.cast_unit())).ok_or("Image pixels unavailable")?;
    let pixels = match buffer {
        SharedImageBuffer::RGB8(pixels) => slint::Image::from_rgb8(pixels),
        SharedImageBuffer::RGBA8(pixels) => slint::Image::from_rgba8(pixels),
        SharedImageBuffer::RGBA8Premultiplied(pixels) => slint::Image::from_rgba8_premultiplied(pixels),
    };
    let mut pixels = pixels.to_rgba8().ok_or("Image cannot be converted to RGBA")?;
    match value.colorize() {
        slint::Brush::SolidColor(tint) if tint.alpha() != 0 => {
            for pixel in pixels.make_mut_slice() {
                pixel.r = tint.red(); pixel.g = tint.green(); pixel.b = tint.blue();
                pixel.a = ((u16::from(pixel.a) * u16::from(tint.alpha()) + 127) / 255) as u8;
            }
        }
        slint::Brush::SolidColor(_) => {}
        _ => return Err("Gradient image colorization is not supported".into()),
    }
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, pixels.width(), pixels.height());
        encoder.set_color(png::ColorType::Rgba); encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
        writer.write_image_data(pixels.as_bytes()).map_err(|error| error.to_string())?;
        writer.finish().map_err(|error| error.to_string())?;
    }
    node.kind = "image".into();
    node.text = Some(format!("data:image/png;base64,{}", STANDARD.encode(png)));
    node.styles.insert("object-fit".into(), match value.image_fit() {
        items::ImageFit::Contain => "contain", items::ImageFit::Cover => "cover", items::ImageFit::Fill => "fill",
        _ => return Err("Unsupported image fit".into()),
    }.into());
    Ok(())
}

fn walk(item: ItemRc, parent: Option<usize>, scene: &mut Scene) {
    let geometry = item.geometry();
    let id = scene.nodes.len();
    let infos = item.element_type_names_and_ids(0).unwrap_or_default();
    let name = infos
        .iter()
        .map(|(kind, name)| {
            if name.is_empty() {
                kind.to_string()
            } else {
                format!("{kind} · {name}")
            }
        })
        .collect::<Vec<_>>()
        .join(" / ");
    let mut node = Node {
        id,
        parent,
        name,
        kind: String::new(),
        rect: [
            geometry.origin.x,
            geometry.origin.y,
            geometry.size.width,
            geometry.size.height,
        ],
        styles: BTreeMap::new(),
        text: None,
    };
    let mut errors = Vec::new();
    let mut caret = None;
    let mut put_brush = |key: &str, brush| match color(brush) {
        Ok(value) => {
            node.styles.insert(key.into(), value);
        }
        Err(error) => errors.push(error),
    };
    if let Some(value) = item.downcast::<items::Rectangle>() {
        node.kind = "rectangle".into();
        put_brush("background", value.as_pin_ref().background());
    } else if let Some(value) = item.downcast::<items::BasicBorderRectangle>() {
        node.kind = "rectangle".into();
        let value = value.as_pin_ref();
        put_brush("background", value.background());
        put_brush("border-color", value.border_color());
        node.styles.insert(
            "border-width".into(),
            format!("{}px", value.border_width().get()),
        );
        node.styles.insert(
            "border-radius".into(),
            format!("{}px", value.border_radius().get()),
        );
    } else if let Some(value) = item.downcast::<items::BorderRectangle>() {
        node.kind = "rectangle".into();
        let value = value.as_pin_ref();
        put_brush("background", value.background());
        put_brush("border-color", value.border_color());
        node.styles.insert(
            "border-width".into(),
            format!("{}px", value.border_width().get()),
        );
        node.styles.insert(
            "border-radius".into(),
            format!(
                "{}px {}px {}px {}px",
                value.border_top_left_radius().get(),
                value.border_top_right_radius().get(),
                value.border_bottom_right_radius().get(),
                value.border_bottom_left_radius().get()
            ),
        );
    } else if let Some(value) = item.downcast::<items::ImageItem>() {
        if let Err(error) = image(value.as_pin_ref(), &mut node) { errors.push(error); }
    } else if let Some(value) = item.downcast::<items::ComplexText>() {
        if let Err(error) = text(value.as_pin_ref(), &item, &mut node) {
            errors.push(error);
        }
    } else if let Some(value) = item.downcast::<items::SimpleText>() {
        if let Err(error) = text(value.as_pin_ref(), &item, &mut node) {
            errors.push(error);
        }
    } else if let Some(value) = item.downcast::<items::TextInput>() {
        use i_slint_core::item_rendering::HasFont;
        let value = value.as_pin_ref();
        let visual = value.visual_representation(None);
        node.kind = "text".into();
        node.text = Some(visual.text.to_string());
        put_brush("color", visual.text_color);
        font_styles(&mut node, value.font_request(&item));
        node.styles.insert(
            "text-align".into(),
            format!("{:?}", value.horizontal_alignment()).to_lowercase(),
        );
        node.styles.insert(
            "vertical-align".into(),
            format!("{:?}", value.vertical_alignment()).to_lowercase(),
        );
        node.styles.insert(
            "white-space".into(),
            if value.wrap() == items::TextWrap::NoWrap {
                "pre"
            } else {
                "pre-wrap"
            }
            .into(),
        );
        node.styles.insert("text-overflow".into(), "clip".into());
        if !visual.selection_range.is_empty() || !visual.preedit_range.is_empty() {
            errors.push("TextInput selection or IME composition is not supported".into());
        }
        if let Some(offset) = visual.cursor_position {
            let adapter = item.window_adapter().expect("Attached item has a window");
            let rect = adapter
                .renderer()
                .text_input_cursor_rect_for_byte_offset(value, &item, offset);
            caret = Some((
                [
                    rect.origin.x,
                    rect.origin.y,
                    rect.size.width,
                    rect.size.height,
                ],
                color(visual.cursor_color.into()).expect("Solid cursor color"),
            ));
        }
    } else if let Some(value) = item.downcast::<items::Clip>() {
        if value.as_pin_ref().clip() && (geometry.size.width <= 0. || geometry.size.height <= 0.) {
            return;
        }
        node.kind = "group".into();
        node.styles.insert(
            "overflow".into(),
            if value.as_pin_ref().clip() {
                "hidden"
            } else {
                "visible"
            }
            .into(),
        );
    } else if let Some(value) = item.downcast::<items::Opacity>() {
        node.kind = "group".into();
        let opacity = value.as_pin_ref().opacity();
        if opacity <= 0. {
            return;
        }
        node.styles.insert("opacity".into(), opacity.to_string());
    } else if let Some(value) = item.downcast::<items::WindowItem>() {
        node.kind = "window".into();
        put_brush("background", value.as_pin_ref().background());
    } else if item.downcast::<items::TouchArea>().is_some()
        || item.downcast::<items::FocusScope>().is_some()
        || item.downcast::<items::Flickable>().is_some()
        || item.downcast::<items::Empty>().is_some()
    {
        node.kind = "group".into();
    } else {
        node.kind = "unsupported".into();
        errors.push(format!("Unsupported item: {:?}", item));
    }
    if node.name.is_empty() {
        node.name = node.kind.clone();
    }
    for error in errors {
        scene.unsupported.push(format!("{}: {error}", node.name));
    }
    scene.nodes.push(node);
    if let Some((rect, color)) = caret {
        scene.nodes.push(Node {
            id: scene.nodes.len(),
            parent: Some(id),
            name: "Input caret".into(),
            kind: "rectangle".into(),
            rect,
            styles: BTreeMap::from([("background".into(), color)]),
            text: None,
        });
    }
    let mut child = item.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        walk(current, Some(id), scene);
    }
}
pub fn capture(window: &slint::Window, name: &str) -> Result<Scene, Box<dyn std::error::Error>> {
    let component = WindowInner::from_pub(window).component();
    i_slint_core::item_tree::ensure_item_tree_instantiated(&component);
    let mut scene = Scene {
        schema: 1,
        name: name.into(),
        width: window.size().width,
        height: window.size().height,
        nodes: Vec::new(),
        unsupported: Vec::new(),
    };
    walk(ItemRc::new_root(component), None, &mut scene);
    Ok(scene)
}

pub fn activate(window: &slint::Window, label: &str) -> Result<(), Box<dyn std::error::Error>> {
    fn find(item: ItemRc, label: &str) -> Option<slint::LogicalPosition> {
        if item.is_visible()
            && item
                .accessible_string_property(
                    i_slint_core::accessibility::AccessibleStringProperty::Label,
                )
                .as_deref()
                == Some(label)
        {
            let center = item.map_to_window(item.geometry().center());
            return Some(slint::LogicalPosition::new(center.x, center.y));
        }
        let mut child = item.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            if let Some(point) = find(current, label) {
                return Some(point);
            }
        }
        None
    }
    let component = WindowInner::from_pub(window).component();
    let position = find(ItemRc::new_root(component), label)
        .ok_or_else(|| format!("No visible control labelled {label:?}"))?;
    use slint::platform::{PointerEventButton, WindowEvent};
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
    Ok(())
}
