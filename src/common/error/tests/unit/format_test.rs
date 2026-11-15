use std::any::Any;

use snafu::prelude::*;
use snafu::GenerateImplicitData;
use snafu::Location;

use super::*;
use crate::ext::StackError;

#[derive(Debug, Snafu)]
#[snafu(display("This is a leaf error"))]
struct Leaf;

impl ErrorExt for Leaf {
    fn location_opt(&self) -> Option<Location> {
        None
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl StackError for Leaf {
    fn debug_fmt(&self, _: usize, _: &mut Vec<String>) {}

    fn next(&self) -> Option<&dyn StackError> {
        None
    }
}

#[derive(Debug, Snafu)]
#[snafu(display("This is a leaf with location"))]
struct LeafWithLocation {
    location: Location,
}

impl ErrorExt for LeafWithLocation {
    fn location_opt(&self) -> Option<Location> {
        None
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl StackError for LeafWithLocation {
    fn debug_fmt(&self, _: usize, _: &mut Vec<String>) {}

    fn next(&self) -> Option<&dyn StackError> {
        None
    }
}

#[derive(Debug, Snafu)]
#[snafu(display("Internal error"))]
struct Internal {
    #[snafu(source)]
    source: Leaf,
    location: Location,
}

impl ErrorExt for Internal {
    fn location_opt(&self) -> Option<Location> {
        None
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl StackError for Internal {
    fn debug_fmt(&self, layer: usize, buf: &mut Vec<String>) {
        buf.push(format!("{}: Internal error, at {}", layer, self.location));
        self.source.debug_fmt(layer + 1, buf);
    }

    fn next(&self) -> Option<&dyn StackError> {
        Some(&self.source)
    }
}

#[test]
fn test_debug_format() {
    let err = Leaf;

    let msg = format!("{:?}", DebugFormat::new(&err));
    assert_eq!("This is a leaf error.", msg);

    let err = LeafWithLocation {
        location: Location::generate(),
    };

    // TODO(ruihang): display location here
    let msg = format!("{:?}", DebugFormat::new(&err));
    assert!(msg.starts_with("This is a leaf with location."));

    let err = Internal {
        source: Leaf,
        location: Location::generate(),
    };

    // TODO(ruihang): display location here
    let msg = format!("{:?}", DebugFormat::new(&err));
    assert!(msg.contains("Internal error. Caused by: Leaf"));
}
