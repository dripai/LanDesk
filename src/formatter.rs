use crate::protocol::valid_code_edit;
use objc2::{AnyThread, Message, define_class, msg_send, rc::Retained, runtime::AnyObject};
use objc2_foundation::{NSFormatter, NSObjectProtocol, NSRange, NSString};

// An NSString formatter preserves leading zeroes; NSNumberFormatter would parse a number.
define_class!(
    #[unsafe(super = NSFormatter)]
    #[thread_kind = AnyThread]
    pub struct CodeFormatter;
    unsafe impl NSObjectProtocol for CodeFormatter {}
    impl CodeFormatter {
        #[unsafe(method_id(stringForObjectValue:))]
        fn string_for_object(&self, object: Option<&AnyObject>) -> Option<Retained<NSString>> {
            object.and_then(|object| object.downcast_ref::<NSString>()).map(|string| string.retain())
        }
        #[unsafe(method(getObjectValue:forString:errorDescription:))]
        fn object_for_string(&self, object: *mut *mut AnyObject, string: &NSString, _error: *mut *mut NSString) -> bool {
            let accepted = valid_code_edit(&string.to_string());
            // AppKit owns these out pointers and retains the accepted NSString value.
            if accepted && !object.is_null() {
                unsafe { *object = (string as *const NSString).cast_mut().cast(); }
            }
            accepted
        }
        #[unsafe(method(isPartialStringValid:proposedSelectedRange:originalString:originalSelectedRange:errorDescription:))]
        fn partial_valid(&self, proposed: *mut *mut NSString, _selection: *mut NSRange, _original: &NSString, _original_selection: NSRange, _error: *mut *mut NSString) -> bool {
            // Reject the whole edit, including paste, without moving AppKit's selection.
            let string = if proposed.is_null() { None } else { unsafe { (*proposed).as_ref() } };
            string.is_some_and(|string| valid_code_edit(&string.to_string()))
        }
    }
);

impl CodeFormatter {
    pub fn new() -> Retained<Self> {
        unsafe { msg_send![Self::alloc(), init] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_formatter_rejects_letters_paste_and_seventh_digit() {
        objc2::rc::autoreleasepool(|_| {
            let formatter = CodeFormatter::new();
            let original = NSString::from_str("001234");
            for (input, accepted) in [
                ("", true),
                ("0", true),
                ("001234", true),
                ("0012345", false),
                ("12a345", false),
                ("12 345", false),
                ("123\n45", false),
                ("１２３４５６", false),
            ] {
                let mut proposed = NSString::from_str(input);
                let valid = unsafe {
                    formatter.isPartialStringValid_proposedSelectedRange_originalString_originalSelectedRange_errorDescription(
                    &mut proposed, std::ptr::null_mut(), &original, NSRange::new(0, 6), None,
                )
                };
                assert_eq!(valid, accepted, "{input:?}");
                assert_eq!(proposed.to_string(), input);
            }
            let mut object = None;
            assert!(unsafe {
                formatter.getObjectValue_forString_errorDescription(
                    Some(&mut object),
                    &original,
                    None,
                )
            });
            let rendered = unsafe { formatter.stringForObjectValue(object.as_deref()) }.unwrap();
            assert_eq!(rendered.to_string(), "001234");
        });
    }
}
