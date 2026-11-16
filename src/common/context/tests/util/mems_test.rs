use common_context::util::mems::{MemCopy, MemCopyError};

#[test]
fn copies_complete_and_partial_buffers() {
    let source = [1, 2, 3, 4, 5];
    let mut complete = [0; 5];
    let mut partial = [0; 3];
    MemCopy::fast_copy(&mut complete, &source).unwrap();
    assert_eq!(complete, source);
    assert_eq!(MemCopy::partial_copy(&mut partial, &source), 3);
    assert_eq!(partial, [1, 2, 3]);
}

#[test]
fn rejects_small_destination() {
    let mut destination = [0; 2];
    let error = MemCopy::safe_copy(&mut destination, &[1, 2, 3]).unwrap_err();
    assert!(matches!(
        error,
        MemCopyError::DestTooSmall {
            dest_len: 2,
            src_len: 3
        }
    ));
}
