//! Socket type system.
//!
//! Every node input/output is typed. The graph executor uses these types to
//! validate connections (only same-typed sockets may be wired) and the canvas
//! widget uses them to color socket circles and connection bezier curves.

/// Logical type carried over a graph edge.
///
/// `None` is the type of an empty/unset value — used as a placeholder for
/// optional inputs that have not been wired up. Type-mismatched connections
/// (e.g. wiring a `Number` source into a `Geometry3d` sink) are rejected by
/// `Graph::connect`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SocketType {
    None,
    Number,
    Bool,
    StringVal,
    Color,
    Matrix4x4,
    Path2d,
    Geometry3d,
}

impl SocketType {
    /// Display color for a socket of this type, used by the node canvas.
    /// Returned as RGBA in the 0..=255 byte range so callers can convert to
    /// whatever their rendering layer prefers without a dependency on
    /// `agg-gui`'s color type from this crate.
    pub fn display_color_rgba(self) -> [u8; 4] {
        match self {
            SocketType::None => [128, 128, 128, 255],
            SocketType::Number => [70, 130, 220, 255],   // blue
            SocketType::Bool => [220, 200, 60, 255],     // yellow
            SocketType::StringVal => [240, 240, 240, 255], // white
            SocketType::Color => [200, 100, 200, 255],   // magenta-ish (canvas paints rainbow over this)
            SocketType::Matrix4x4 => [150, 80, 200, 255], // purple
            SocketType::Path2d => [80, 200, 100, 255],   // green
            SocketType::Geometry3d => [240, 140, 40, 255], // orange
        }
    }

    /// Returns true when a value of `from` (= `self`) may be wired into a
    /// socket of type `other`.
    ///
    /// Rules:
    /// - Exact type match: always allowed.
    /// - Target type `None`: wildcard. `None` is the placeholder for an
    ///   un-typed input slot — the canonical use is the trailing empty
    ///   slot on the Output node, which adopts the source's type on
    ///   connect. Any source type is allowed to land on a `None` input.
    ///
    /// Promotions between concrete types (e.g. Number → Color) are not
    /// supported and would belong on dedicated converter nodes.
    pub fn is_compatible_with(self, other: SocketType) -> bool {
        self == other || other == SocketType::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatibility_is_exact_match() {
        assert!(SocketType::Number.is_compatible_with(SocketType::Number));
        assert!(!SocketType::Number.is_compatible_with(SocketType::Geometry3d));
        assert!(!SocketType::Path2d.is_compatible_with(SocketType::Geometry3d));
    }

    #[test]
    fn target_none_accepts_any_source() {
        // None on the target side is the placeholder type used by the
        // Output node's trailing empty input slot. Any source must be
        // allowed; the node's on_input_connected hook retypes the slot
        // to match the source.
        for src in [
            SocketType::Number,
            SocketType::Bool,
            SocketType::StringVal,
            SocketType::Color,
            SocketType::Matrix4x4,
            SocketType::Path2d,
            SocketType::Geometry3d,
        ] {
            assert!(
                src.is_compatible_with(SocketType::None),
                "{:?} should be allowed into a None placeholder slot",
                src,
            );
        }
    }

    #[test]
    fn source_none_does_not_satisfy_concrete_targets() {
        // The wildcard rule is one-directional: a source of type None
        // (which shouldn't really exist — outputs are always concrete)
        // does NOT satisfy a concrete-typed input.
        assert!(!SocketType::None.is_compatible_with(SocketType::Number));
        assert!(!SocketType::None.is_compatible_with(SocketType::Geometry3d));
    }

    #[test]
    fn each_type_has_a_color() {
        // Smoke-test that no variant returns the same alpha-zero color
        let types = [
            SocketType::None,
            SocketType::Number,
            SocketType::Bool,
            SocketType::StringVal,
            SocketType::Color,
            SocketType::Matrix4x4,
            SocketType::Path2d,
            SocketType::Geometry3d,
        ];
        for t in types {
            assert_eq!(t.display_color_rgba()[3], 255, "{:?} alpha should be opaque", t);
        }
    }
}
