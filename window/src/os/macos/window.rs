use super::nsstring;
use crate::bitmaps::Image;
use crate::os::macos::bitmap::BitmapRef;
use crate::{
    BitmapImage, Color, Dimensions, Modifiers, MouseButtons, MouseEvent, MouseEventKind,
    MousePress, Operator, PaintContext, WindowCallbacks,
};
use cocoa::appkit::{
    NSApplicationActivateIgnoringOtherApps, NSBackingStoreBuffered, NSEvent, NSEventModifierFlags,
    NSRunningApplication, NSView, NSViewHeightSizable, NSViewWidthSizable, NSWindow,
    NSWindowStyleMask,
};
use cocoa::base::*;
use cocoa::foundation::{NSPoint, NSRect, NSSize};
use core_graphics::image::CGImageRef;
use failure::Fallible;
use objc::declare::ClassDecl;
use objc::rc::{StrongPtr, WeakPtr};
use objc::runtime::{Class, Object, Sel};
use objc::*;
use std::cell::RefCell;
use std::ffi::c_void;
use std::rc::Rc;

pub struct Window {
    window: StrongPtr,
    view: StrongPtr,
}

impl Drop for Window {
    fn drop(&mut self) {
        eprintln!("drop Window");
    }
}

impl Window {
    pub fn new_window(
        _class_name: &str,
        name: &str,
        width: usize,
        height: usize,
        callbacks: Box<WindowCallbacks>,
    ) -> Fallible<Window> {
        unsafe {
            let style_mask = NSWindowStyleMask::NSTitledWindowMask
                | NSWindowStyleMask::NSClosableWindowMask
                | NSWindowStyleMask::NSMiniaturizableWindowMask
                | NSWindowStyleMask::NSResizableWindowMask;
            let rect = NSRect::new(
                NSPoint::new(0., 0.),
                NSSize::new(width as f64, height as f64),
            );

            let inner = Rc::new(RefCell::new(Inner {
                callbacks,
                view_id: None,
            }));

            let window = StrongPtr::new(
                NSWindow::alloc(nil).initWithContentRect_styleMask_backing_defer_(
                    rect,
                    style_mask,
                    NSBackingStoreBuffered,
                    NO,
                ),
            );

            window.cascadeTopLeftFromPoint_(NSPoint::new(20.0, 20.0));
            window.setTitle_(*nsstring(&name));
            window.setAcceptsMouseMovedEvents_(YES);

            let view = WindowView::alloc(&inner)?;
            view.initWithFrame_(rect);
            view.setAutoresizingMask_(NSViewHeightSizable | NSViewWidthSizable);

            window.setContentView_(*view);
            window.setDelegate_(*view);

            Ok(Self { window, view })
        }
    }

    pub fn show(&self) {
        unsafe {
            let current_app = NSRunningApplication::currentApplication(nil);
            current_app.activateWithOptions_(NSApplicationActivateIgnoringOtherApps);
            self.window.makeKeyAndOrderFront_(nil)
        }
    }
}

struct Inner {
    callbacks: Box<WindowCallbacks>,
    view_id: Option<WeakPtr>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        eprintln!("dropping Inner");
    }
}

const CLS_NAME: &str = "WezTermWindowView";

struct WindowView {
    inner: Rc<RefCell<Inner>>,
}

impl Drop for WindowView {
    fn drop(&mut self) {
        eprintln!("dropping WindowView");
    }
}

pub fn superclass(this: &Object) -> &'static Class {
    unsafe {
        let superclass: id = msg_send![this, superclass];
        &*(superclass as *const _)
    }
}

struct MacGraphicsContext<'a> {
    buffer: &'a mut BitmapImage,
}

impl<'a> PaintContext for MacGraphicsContext<'a> {
    fn clear_rect(
        &mut self,
        dest_x: isize,
        dest_y: isize,
        width: usize,
        height: usize,
        color: Color,
    ) {
        self.buffer.clear_rect(dest_x, dest_y, width, height, color)
    }

    fn clear(&mut self, color: Color) {
        self.buffer.clear(color);
    }

    fn get_dimensions(&self) -> Dimensions {
        let (pixel_width, pixel_height) = self.buffer.image_dimensions();
        Dimensions {
            pixel_width,
            pixel_height,
            dpi: 96,
        }
    }

    fn draw_image_subset(
        &mut self,
        dest_x: isize,
        dest_y: isize,
        src_x: usize,
        src_y: usize,
        width: usize,
        height: usize,
        im: &dyn BitmapImage,
        operator: Operator,
    ) {
        self.buffer
            .draw_image_subset(dest_x, dest_y, src_x, src_y, width, height, im, operator)
    }

    fn draw_line(
        &mut self,
        start_x: isize,
        start_y: isize,
        dest_x: isize,
        dest_y: isize,
        color: Color,
        operator: Operator,
    ) {
        self.buffer
            .draw_line(start_x, start_y, dest_x, dest_y, color, operator);
    }
}

fn decode_mouse_buttons(mask: u64) -> MouseButtons {
    let mut buttons = MouseButtons::NONE;

    if (mask & (1 << 0)) != 0 {
        buttons |= MouseButtons::LEFT;
    }
    if (mask & (1 << 1)) != 0 {
        buttons |= MouseButtons::RIGHT;
    }
    if (mask & (1 << 2)) != 0 {
        buttons |= MouseButtons::MIDDLE;
    }
    if (mask & (1 << 3)) != 0 {
        buttons |= MouseButtons::X1;
    }
    if (mask & (1 << 4)) != 0 {
        buttons |= MouseButtons::X2;
    }
    buttons
}

fn key_modifiers(flags: NSEventModifierFlags) -> Modifiers {
    let mut mods = Modifiers::NONE;

    if flags.contains(NSEventModifierFlags::NSShiftKeyMask) {
        mods |= Modifiers::SHIFT;
    }
    if flags.contains(NSEventModifierFlags::NSAlternateKeyMask) {
        mods |= Modifiers::ALT;
    }
    if flags.contains(NSEventModifierFlags::NSControlKeyMask) {
        mods |= Modifiers::CTRL;
    }
    if flags.contains(NSEventModifierFlags::NSCommandKeyMask) {
        mods |= Modifiers::SUPER;
    }

    mods
}

impl WindowView {
    fn view_id(&self) -> id {
        let inner = self.inner.borrow_mut();
        inner.view_id.as_ref().map(|w| *w.load()).unwrap_or(nil)
    }

    fn window_id(&self) -> id {
        unsafe { msg_send![self.view_id(), window] }
    }

    extern "C" fn dealloc(this: &mut Object, _sel: Sel) {
        eprintln!("WindowView::dealloc");
        Self::drop_inner(this);
        unsafe {
            let superclass = superclass(this);
            let () = msg_send![super(this, superclass), dealloc];
        }
    }

    /// `dealloc` is called when our NSView descendant is destroyed.
    /// In practice, I've not seen this trigger, which likely means
    /// that there is something afoot with reference counting.
    /// The cardinality of Window and View objects is low enough
    /// that I'm "OK" with this for now.
    /// What really matters is that the `Inner` object is dropped
    /// in a timely fashion once the window is closed, so we manage
    /// that by hooking into `windowWillClose` and routing both
    /// `dealloc` and `windowWillClose` to `drop_inner`.
    fn drop_inner(this: &mut Object) {
        unsafe {
            let myself: *mut c_void = *this.get_ivar(CLS_NAME);
            this.set_ivar(CLS_NAME, std::ptr::null_mut() as *mut c_void);

            if !myself.is_null() {
                let myself = Box::from_raw(myself as *mut Self);
                drop(myself);
            }
        }
    }

    extern "C" fn accepts_first_responder(_this: &mut Object, _sel: Sel) -> BOOL {
        YES
    }

    extern "C" fn window_should_close(this: &mut Object, _sel: Sel, _id: id) -> BOOL {
        eprintln!("window_should_close");

        unsafe {
            let () = msg_send![this, setNeedsDisplay: YES];
        }

        if let Some(this) = Self::get_this(this) {
            if this.inner.borrow_mut().callbacks.can_close() {
                YES
            } else {
                NO
            }
        } else {
            YES
        }
    }

    // Switch the coordinate system to have 0,0 in the top left
    extern "C" fn is_flipped(_this: &Object, _sel: Sel) -> BOOL {
        YES
    }

    extern "C" fn window_will_close(this: &mut Object, _sel: Sel, _id: id) {
        eprintln!("window_will_close");
        if let Some(this) = Self::get_this(this) {
            // Advise the window of its impending death
            this.inner.borrow_mut().callbacks.destroy();
        }

        // Release and zero out the inner member
        Self::drop_inner(this);
    }

    fn mouse_common(this: &mut Object, nsevent: id, kind: MouseEventKind) {
        let view = this as id;
        let coords;
        let mouse_buttons;
        let modifiers;
        unsafe {
            coords = NSView::convertPoint_fromView_(view, nsevent.locationInWindow(), nil);
            mouse_buttons = decode_mouse_buttons(NSEvent::pressedMouseButtons(nsevent));
            modifiers = key_modifiers(nsevent.modifierFlags());
        }
        let event = MouseEvent {
            kind,
            x: coords.x.max(0.0) as u16,
            y: coords.y.max(0.0) as u16,
            mouse_buttons,
            modifiers,
        };

        if let Some(myself) = Self::get_this(this) {
            eprintln!("{:?}", event);
            // myself.inner.borrow_mut().callbacks.mouse_event(&event, &window);
        }
    }

    extern "C" fn mouse_up(this: &mut Object, _sel: Sel, nsevent: id) {
        Self::mouse_common(this, nsevent, MouseEventKind::Release(MousePress::Left));
    }

    extern "C" fn mouse_down(this: &mut Object, _sel: Sel, nsevent: id) {
        Self::mouse_common(this, nsevent, MouseEventKind::Press(MousePress::Left));
    }
    extern "C" fn right_mouse_up(this: &mut Object, _sel: Sel, nsevent: id) {
        Self::mouse_common(this, nsevent, MouseEventKind::Release(MousePress::Right));
    }

    extern "C" fn right_mouse_down(this: &mut Object, _sel: Sel, nsevent: id) {
        Self::mouse_common(this, nsevent, MouseEventKind::Press(MousePress::Right));
    }

    extern "C" fn mouse_moved_or_dragged(this: &mut Object, _sel: Sel, nsevent: id) {
        Self::mouse_common(this, nsevent, MouseEventKind::Move);
    }

    extern "C" fn did_resize(this: &mut Object, _sel: Sel, notification: id) {
        let frame = unsafe { NSView::frame(this as *mut _) };
        let width = frame.size.width;
        let height = frame.size.height;
        if let Some(this) = Self::get_this(this) {
            this.inner.borrow_mut().callbacks.resize(Dimensions {
                pixel_width: width as usize,
                pixel_height: height as usize,
                dpi: 96,
            });
        }
    }

    extern "C" fn draw_rect(this: &mut Object, _sel: Sel, _dirty_rect: NSRect) {
        let frame = unsafe { NSView::frame(this as *mut _) };
        let width = frame.size.width;
        let height = frame.size.height;

        let mut im = Image::new(width as usize, height as usize);

        if let Some(this) = Self::get_this(this) {
            let mut ctx = MacGraphicsContext { buffer: &mut im };
            this.inner.borrow_mut().callbacks.paint(&mut ctx);
        }

        let cg_image = BitmapRef::with_image(&im);

        fn nsimage_from_cgimage(cg: &CGImageRef, size: NSSize) -> StrongPtr {
            unsafe {
                let ns_image: id = msg_send![class!(NSImage), alloc];
                StrongPtr::new(msg_send![ns_image, initWithCGImage: cg size:size])
            }
        }

        let ns_image = nsimage_from_cgimage(cg_image.as_ref(), NSSize::new(0., 0.));

        unsafe {
            let () = msg_send![*ns_image, drawInRect: frame];
        }
    }

    fn get_this(this: &Object) -> Option<&mut Self> {
        unsafe {
            let myself: *mut c_void = *this.get_ivar(CLS_NAME);
            if myself.is_null() {
                None
            } else {
                Some(&mut *(myself as *mut Self))
            }
        }
    }

    fn alloc(inner: &Rc<RefCell<Inner>>) -> Fallible<StrongPtr> {
        let cls = Self::get_class();

        let view_id: StrongPtr = unsafe { StrongPtr::new(msg_send![cls, new]) };

        inner.borrow_mut().view_id.replace(view_id.weak());

        let view = Box::into_raw(Box::new(Self {
            inner: Rc::clone(&inner),
        }));

        unsafe {
            (**view_id).set_ivar(CLS_NAME, view as *mut c_void);
        }

        Ok(view_id)
    }

    fn get_class() -> &'static Class {
        Class::get(CLS_NAME).unwrap_or_else(Self::define_class)
    }

    fn define_class() -> &'static Class {
        let mut cls =
            ClassDecl::new(CLS_NAME, class!(NSView)).expect("Unable to register WindowView class");

        cls.add_ivar::<*mut c_void>(CLS_NAME);

        unsafe {
            cls.add_method(
                sel!(dealloc),
                WindowView::dealloc as extern "C" fn(&mut Object, Sel),
            );

            cls.add_method(
                sel!(windowWillClose:),
                Self::window_will_close as extern "C" fn(&mut Object, Sel, id),
            );

            cls.add_method(
                sel!(windowShouldClose:),
                Self::window_should_close as extern "C" fn(&mut Object, Sel, id) -> BOOL,
            );

            cls.add_method(
                sel!(drawRect:),
                Self::draw_rect as extern "C" fn(&mut Object, Sel, NSRect),
            );

            cls.add_method(
                sel!(isFlipped),
                Self::is_flipped as extern "C" fn(&Object, Sel) -> BOOL,
            );

            cls.add_method(
                sel!(windowDidResize:),
                Self::did_resize as extern "C" fn(&mut Object, Sel, id),
            );

            cls.add_method(
                sel!(mouseMoved:),
                Self::mouse_moved_or_dragged as extern "C" fn(&mut Object, Sel, id),
            );
            cls.add_method(
                sel!(mouseDragged:),
                Self::mouse_moved_or_dragged as extern "C" fn(&mut Object, Sel, id),
            );
            cls.add_method(
                sel!(rightMouseDragged:),
                Self::mouse_moved_or_dragged as extern "C" fn(&mut Object, Sel, id),
            );
            cls.add_method(
                sel!(mouseDown:),
                Self::mouse_down as extern "C" fn(&mut Object, Sel, id),
            );
            cls.add_method(
                sel!(mouseUp:),
                Self::mouse_up as extern "C" fn(&mut Object, Sel, id),
            );
            cls.add_method(
                sel!(rightMouseDown:),
                Self::right_mouse_down as extern "C" fn(&mut Object, Sel, id),
            );
            cls.add_method(
                sel!(rightMouseUp:),
                Self::right_mouse_up as extern "C" fn(&mut Object, Sel, id),
            );

            cls.add_method(
                sel!(acceptsFirstResponder),
                Self::accepts_first_responder as extern "C" fn(&mut Object, Sel) -> BOOL,
            );
        }

        cls.register()
    }
}