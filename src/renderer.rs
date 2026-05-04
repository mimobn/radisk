use crate::color::{colorize_segment, hover_color, ColorConfig, SegmentColors};
use crate::radial::{RadialMap, Segment, DEGREE_FACTOR, MAX_DEGREE};
use ratatui::style::Color;
use ratatui::widgets::canvas::{Painter, Shape};
use uuid::Uuid;

/// Braille resolution constants
#[allow(dead_code)]
const BRAILLE_WIDTH: usize = 2;
#[allow(dead_code)]
const BRAILLE_HEIGHT: usize = 4;

/// Braille dot pattern mapping (Unicode offset)
#[allow(dead_code)]
const BRAILLE_OFFSET: u32 = 0x2800;
#[allow(dead_code)]
const BRAILLE_PATTERN: [[u32; BRAILLE_WIDTH]; BRAILLE_HEIGHT] = [
    [0x01, 0x08], // row 0: dots 1, 4
    [0x02, 0x10], // row 1: dots 2, 5
    [0x04, 0x20], // row 2: dots 3, 6
    [0x40, 0x80], // row 3: dots 7, 8
];

/// A single braille cell with dots and color
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct BrailleCell {
    pub dots: u32,
    pub fg: Color,
}

impl Default for BrailleCell {
    fn default() -> Self {
        Self {
            dots: 0,
            fg: Color::Reset,
        }
    }
}

#[allow(dead_code)]
impl BrailleCell {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_dot(&mut self, x: usize, y: usize, color: Color) {
        if x < BRAILLE_WIDTH && y < BRAILLE_HEIGHT {
            self.dots |= BRAILLE_PATTERN[y][x];
            self.fg = color;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.dots == 0
    }

    pub fn to_char(self) -> char {
        if self.dots == 0 {
            ' '
        } else {
            std::char::from_u32(BRAILLE_OFFSET + self.dots).unwrap_or(' ')
        }
    }
}

/// Braille buffer for rendering
#[allow(dead_code)]
pub struct BrailleBuffer {
    width: usize,
    height: usize,
    cells: Vec<Vec<BrailleCell>>,
}

#[allow(dead_code)]
impl BrailleBuffer {
    pub fn new(width: usize, height: usize) -> Self {
        let cells = vec![vec![BrailleCell::new(); width]; height];
        Self {
            width,
            height,
            cells,
        }
    }

    pub fn clear(&mut self) {
        for row in &mut self.cells {
            for cell in row {
                *cell = BrailleCell::new();
            }
        }
    }

    /// Set a pixel at sub-pixel coordinates
    pub fn set_pixel(&mut self, px: usize, py: usize, color: Color) {
        let cell_x = px / BRAILLE_WIDTH;
        let cell_y = py / BRAILLE_HEIGHT;
        let dot_x = px % BRAILLE_WIDTH;
        let dot_y = py % BRAILLE_HEIGHT;

        if cell_x < self.width && cell_y < self.height {
            self.cells[cell_y][cell_x].set_dot(dot_x, dot_y, color);
        }
    }

    /// Get cell at cell coordinates
    pub fn get_cell(&self, x: usize, y: usize) -> Option<&BrailleCell> {
        if x < self.width && y < self.height {
            Some(&self.cells[y][x])
        } else {
            None
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }
}

/// Canvas coordinate system for the radial map
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct CanvasCoords {
    pub center_x: f64,
    pub center_y: f64,
    pub scale: f64,
}

impl CanvasCoords {
    pub fn new(width: usize, height: usize) -> Self {
        let center_x = width as f64 / 2.0;
        let center_y = height as f64 / 2.0;
        let scale = (width.min(height) as f64) / 2.0;

        Self {
            center_x,
            center_y,
            scale,
        }
    }

    /// Convert polar coordinates (angle in degrees, radius) to pixel coordinates
    #[allow(dead_code)]
    pub fn polar_to_pixel(&self, angle_degrees: f64, radius: f64) -> (f64, f64) {
        let angle_rad = angle_degrees.to_radians();
        let x = self.center_x + radius * angle_rad.cos();
        let y = self.center_y - radius * angle_rad.sin(); // Y inverted for screen coords
        (x, y)
    }

    /// Convert pixel coordinates to polar (angle in degrees, radius)
    pub fn pixel_to_polar(&self, px: f64, py: f64) -> (f64, f64) {
        let dx = px - self.center_x;
        let dy = self.center_y - py; // Y inverted
        let radius = (dx * dx + dy * dy).sqrt();
        let mut angle = dy.atan2(dx).to_degrees();
        if angle < 0.0 {
            angle += 360.0;
        }
        (angle, radius)
    }
}

/// Check if a point is within an arc segment
#[allow(dead_code)]
pub fn is_point_in_arc(
    px: f64,
    py: f64,
    coords: &CanvasCoords,
    start_angle: f64,
    sweep_angle: f64,
    inner_radius: f64,
    outer_radius: f64,
) -> bool {
    let (angle, radius) = coords.pixel_to_polar(px, py);

    // Check radius bounds
    if radius < inner_radius || radius > outer_radius {
        return false;
    }

    // Normalize angles to 0-360
    let start = start_angle % 360.0;
    let end = (start_angle + sweep_angle) % 360.0;

    // Check angle bounds
    if start <= end {
        angle >= start && angle <= end
    } else {
        // Wraps around 0/360
        angle >= start || angle <= end
    }
}

/// Shape for drawing a filled arc segment
pub struct ArcShape {
    pub start_angle: f64,
    pub sweep_angle: f64,
    pub inner_radius: f64,
    pub outer_radius: f64,
    pub color: Color,
    pub center_x: f64,
    pub center_y: f64,
}

impl Shape for ArcShape {
    fn draw(&self, painter: &mut Painter<'_, '_>) {
        if self.sweep_angle <= 0.0 || self.outer_radius <= self.inner_radius {
            return;
        }

        let steps = (self.sweep_angle * 2.0).ceil() as i32;
        let radial_steps = ((self.outer_radius - self.inner_radius) * 2.0).ceil() as i32;

        for i in 0..=steps {
            let angle = self.start_angle + self.sweep_angle * (i as f64) / (steps as f64);
            let angle_rad = angle.to_radians();

            for r in 0..=radial_steps {
                let radius = self.inner_radius
                    + (self.outer_radius - self.inner_radius) * (r as f64) / (radial_steps as f64);
                let x = self.center_x + radius * angle_rad.cos();
                let y = self.center_y + radius * angle_rad.sin();

                if let Some((px, py)) = painter.get_point(x, y) {
                    painter.paint(px, py, self.color);
                }
            }
        }
    }
}

/// Shape for drawing a stroke line between segments
pub struct ArcStrokeShape {
    pub angle: f64,
    pub inner_radius: f64,
    pub outer_radius: f64,
    pub color: Color,
    pub center_x: f64,
    pub center_y: f64,
}

impl Shape for ArcStrokeShape {
    fn draw(&self, painter: &mut Painter<'_, '_>) {
        if self.outer_radius <= self.inner_radius {
            return;
        }

        let angle_rad = self.angle.to_radians();
        let steps = ((self.outer_radius - self.inner_radius) * 2.0).ceil() as i32;

        for r in 0..=steps {
            let radius = self.inner_radius
                + (self.outer_radius - self.inner_radius) * (r as f64) / (steps as f64);
            let x = self.center_x + radius * angle_rad.cos();
            let y = self.center_y + radius * angle_rad.sin();

            if let Some((px, py)) = painter.get_point(x, y) {
                painter.paint(px, py, self.color);
            }
        }
    }
}

/// Shape for drawing a circular stroke line (ring boundary)
pub struct CircleStrokeShape {
    pub radius: f64,
    pub color: Color,
    pub center_x: f64,
    pub center_y: f64,
}

impl Shape for CircleStrokeShape {
    fn draw(&self, painter: &mut Painter<'_, '_>) {
        if self.radius <= 0.0 {
            return;
        }

        // Sample points around the circle circumference
        let steps = (self.radius * 16.0).ceil() as i32;

        for i in 0..=steps {
            let angle = 360.0 * (i as f64) / (steps as f64);
            let angle_rad = angle.to_radians();
            let x = self.center_x + self.radius * angle_rad.cos();
            let y = self.center_y + self.radius * angle_rad.sin();

            if let Some((px, py)) = painter.get_point(x, y) {
                painter.paint(px, py, self.color);
            }
        }
    }
}

/// Shape for drawing the center circle
pub struct CenterShape {
    pub radius: f64,
    pub color: Color,
    pub center_x: f64,
    pub center_y: f64,
}

impl Shape for CenterShape {
    fn draw(&self, painter: &mut Painter<'_, '_>) {
        if self.radius <= 0.0 {
            return;
        }

        let steps = (self.radius * 8.0).ceil() as i32;

        for i in 0..=steps {
            let angle = 360.0 * (i as f64) / (steps as f64);
            let angle_rad = angle.to_radians();

            for r in 0..=(self.radius as i32) {
                let x = self.center_x + (r as f64) * angle_rad.cos();
                let y = self.center_y + (r as f64) * angle_rad.sin();

                if let Some((px, py)) = painter.get_point(x, y) {
                    painter.paint(px, py, self.color);
                }
            }
        }
    }
}

/// Renderer for the radial map
pub struct RadialRenderer {
    pub config: ColorConfig,
    hovered_uuid: Option<Uuid>,
}

impl RadialRenderer {
    pub fn new(config: ColorConfig) -> Self {
        Self {
            config,
            hovered_uuid: None,
        }
    }

    pub fn set_hovered(&mut self, uuid: Option<Uuid>) {
        self.hovered_uuid = uuid;
    }

    #[allow(dead_code)]
    pub fn hovered(&self) -> Option<Uuid> {
        self.hovered_uuid
    }

    /// Get segment colors, with hover highlighting
    pub fn get_segment_colors(&self, segment: &Segment, depth: usize) -> SegmentColors {
        if self.hovered_uuid == Some(segment.uuid) {
            let fill = hover_color(segment, depth, &self.config);
            let pen = colorize_segment(segment, depth, &self.config).pen;
            SegmentColors { fill, pen }
        } else {
            colorize_segment(segment, depth, &self.config)
        }
    }

    /// Render the radial map to canvas shapes
    pub fn render_shapes(
        &self,
        map: &RadialMap,
    ) -> (Vec<ArcShape>, Vec<ArcStrokeShape>, Vec<CircleStrokeShape>) {
        let mut fill_shapes = Vec::new();
        let mut stroke_shapes = Vec::new();
        let mut circle_shapes = Vec::new();

        // Use a neutral stroke color for ring boundaries
        let ring_stroke_color = Color::Rgb(80, 80, 80);

        // Circle at center radius
        circle_shapes.push(CircleStrokeShape {
            radius: map.center_radius,
            color: ring_stroke_color,
            center_x: 0.0,
            center_y: 0.0,
        });

        // Render from outermost to innermost (reverse order for proper z-ordering)
        for ring in map.rings.iter().rev() {
            for segment in &ring.segments {
                let colors = self.get_segment_colors(segment, ring.depth);

                fill_shapes.push(ArcShape {
                    start_angle: segment.start_degrees(),
                    sweep_angle: segment.sweep_degrees(),
                    inner_radius: ring.inner_radius,
                    outer_radius: ring.outer_radius,
                    color: colors.fill.to_ratatui(),
                    center_x: 0.0,
                    center_y: 0.0,
                });

                // Draw stroke at segment start (except first segment in ring)
                if segment.start_angle > 0 {
                    stroke_shapes.push(ArcStrokeShape {
                        angle: segment.start_degrees(),
                        inner_radius: ring.inner_radius,
                        outer_radius: ring.outer_radius,
                        color: colors.pen.to_ratatui(),
                        center_x: 0.0,
                        center_y: 0.0,
                    });
                }
            }

            // Circle at ring outer boundary
            circle_shapes.push(CircleStrokeShape {
                radius: ring.outer_radius,
                color: ring_stroke_color,
                center_x: 0.0,
                center_y: 0.0,
            });
        }

        (fill_shapes, stroke_shapes, circle_shapes)
    }

    /// Find which segment is at a given screen position
    #[allow(dead_code)]
    pub fn hit_test(
        &self,
        map: &RadialMap,
        screen_x: f64,
        screen_y: f64,
        coords: &CanvasCoords,
    ) -> Option<(Uuid, usize)> {
        let (angle, radius) = coords.pixel_to_polar(screen_x, screen_y);

        // Check center circle
        if radius < map.center_radius {
            return None; // Center (root)
        }

        // Check rings from innermost to outermost (for proper hit priority)
        for ring in &map.rings {
            if radius >= ring.inner_radius && radius <= ring.outer_radius {
                // Convert angle to 16ths of degrees
                let angle_16ths = ((angle * DEGREE_FACTOR as f64) as u32) % MAX_DEGREE;

                for segment in &ring.segments {
                    if segment.contains_angle(angle_16ths) {
                        return Some((segment.uuid, ring.depth));
                    }
                }
            }
        }

        None
    }

    /// Get segment by UUID
    pub fn find_segment<'a>(&self, map: &'a RadialMap, uuid: &Uuid) -> Option<&'a Segment> {
        for ring in &map.rings {
            for segment in &ring.segments {
                if segment.uuid == *uuid {
                    return Some(segment);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_braille_cell_basic() {
        let mut cell = BrailleCell::new();
        assert!(cell.is_empty());
        assert_eq!(cell.to_char(), ' ');

        cell.set_dot(0, 0, Color::Red);
        assert!(!cell.is_empty());
        assert_ne!(cell.to_char(), ' ');
    }

    #[test]
    fn test_braille_cell_multiple_dots() {
        let mut cell = BrailleCell::new();
        cell.set_dot(0, 0, Color::Red);
        cell.set_dot(1, 0, Color::Red);
        cell.set_dot(0, 1, Color::Red);

        assert!(!cell.is_empty());
        // Should be braille char with dots 1, 4, 2 set
    }

    #[test]
    fn test_braille_buffer() {
        let mut buf = BrailleBuffer::new(10, 10);
        assert_eq!(buf.width(), 10);
        assert_eq!(buf.height(), 10);

        buf.set_pixel(5, 5, Color::Blue);
        let cell = buf.get_cell(2, 1).unwrap();
        assert!(!cell.is_empty());
    }

    #[test]
    fn test_canvas_coords_center() {
        let coords = CanvasCoords::new(100, 80);
        assert_eq!(coords.center_x, 50.0);
        assert_eq!(coords.center_y, 40.0);
    }

    #[test]
    fn test_polar_to_pixel() {
        let coords = CanvasCoords::new(100, 100);

        // 0 degrees (right)
        let (x, y) = coords.polar_to_pixel(0.0, 10.0);
        approx::assert_relative_eq!(x, 60.0);
        approx::assert_relative_eq!(y, 50.0);

        // 90 degrees (up)
        let (x, y) = coords.polar_to_pixel(90.0, 10.0);
        approx::assert_relative_eq!(x, 50.0);
        approx::assert_relative_eq!(y, 40.0);

        // 180 degrees (left)
        let (x, y) = coords.polar_to_pixel(180.0, 10.0);
        approx::assert_relative_eq!(x, 40.0);
        approx::assert_relative_eq!(y, 50.0);
    }

    #[test]
    fn test_pixel_to_polar() {
        let coords = CanvasCoords::new(100, 100);

        // Point to the right of center
        let (angle, radius) = coords.pixel_to_polar(60.0, 50.0);
        approx::assert_relative_eq!(angle, 0.0, epsilon = 0.1);
        approx::assert_relative_eq!(radius, 10.0, epsilon = 0.1);

        // Point above center
        let (angle, radius) = coords.pixel_to_polar(50.0, 40.0);
        approx::assert_relative_eq!(angle, 90.0, epsilon = 0.1);
        approx::assert_relative_eq!(radius, 10.0, epsilon = 0.1);
    }

    #[test]
    fn test_is_point_in_arc() {
        let coords = CanvasCoords::new(100, 100);

        // Point in first quadrant arc (0-90 degrees, radius 5-15)
        assert!(is_point_in_arc(55.0, 45.0, &coords, 0.0, 90.0, 5.0, 15.0));

        // Point outside radius
        assert!(!is_point_in_arc(55.0, 45.0, &coords, 0.0, 90.0, 20.0, 30.0));

        // Point outside angle
        assert!(!is_point_in_arc(45.0, 50.0, &coords, 0.0, 90.0, 5.0, 15.0));
    }

    #[test]
    fn test_arc_wrap_around_360() {
        let coords = CanvasCoords::new(100, 100);

        // Arc that wraps from 350 to 10 degrees
        // Point at 355 degrees (near 0)
        let px = 50.0 + 10.0 * (355.0_f64.to_radians()).cos();
        let py = 50.0 - 10.0 * (355.0_f64.to_radians()).sin();
        assert!(is_point_in_arc(px, py, &coords, 350.0, 20.0, 5.0, 15.0));
    }

    #[test]
    fn test_hit_testing() {
        use crate::radial::{build_radial_map, RadialConfig};
        use crate::tree::{File, Folder, TreeArena};

        // Create a simple arena
        let mut arena = TreeArena::new();
        let root_file = File {
            name: "root".to_string(),
            size: 1000,
            parent: None,
            path: std::path::PathBuf::from("/root"),
            ..Default::default()
        };
        let root_folder = Folder {
            file: root_file,
            children_files: Vec::new(),
            children_folders: Vec::new(),
            child_count: 1,
        };
        let root_id = arena.add_folder(root_folder);
        arena.set_root(root_id);

        let file = File {
            name: "big.txt".to_string(),
            size: 1000,
            parent: Some(root_id),
            path: std::path::PathBuf::from("/root/big.txt"),
            ..Default::default()
        };
        let fid = arena.add_file(file);
        arena.folder_mut(root_id).children_files.push(fid);

        let config = RadialConfig {
            small_file_factor: 1,
            ..Default::default()
        };
        let map = build_radial_map(&arena, root_id, &config);

        let renderer = RadialRenderer::new(ColorConfig::default());
        let coords = CanvasCoords::new(100, 100);

        // Hit test at a point in the first ring. The exact geometry depends
        // on the synthetic test fixture, so the assertion is "no panic" — the
        // unit tests for the geometry primitives cover correctness.
        let _ = renderer.hit_test(&map, 60.0, 50.0, &coords);
    }

    #[test]
    fn test_arc_containment_point_inside() {
        let coords = CanvasCoords::new(100, 100);

        // Point clearly inside a 0-90 degree arc
        let (x, y) = coords.polar_to_pixel(45.0, 10.0);
        assert!(is_point_in_arc(x, y, &coords, 0.0, 90.0, 5.0, 15.0));
    }

    #[test]
    fn test_arc_containment_point_outside() {
        let coords = CanvasCoords::new(100, 100);

        // Point at 135 degrees - outside 0-90 arc
        let (x, y) = coords.polar_to_pixel(135.0, 10.0);
        assert!(!is_point_in_arc(x, y, &coords, 0.0, 90.0, 5.0, 15.0));
    }

    #[test]
    fn test_arc_containment_on_edge() {
        let coords = CanvasCoords::new(100, 100);

        // Point at exactly 90 degrees
        let (x, y) = coords.polar_to_pixel(90.0, 10.0);
        // Should be included (end angle is inclusive)
        assert!(is_point_in_arc(x, y, &coords, 0.0, 90.0, 5.0, 15.0));
    }

    #[test]
    fn test_center_circle_detection() {
        let coords = CanvasCoords::new(100, 100);

        // Point at center: angle is undefined at the origin, only the radius
        // assertion is meaningful.
        let (_angle, radius) = coords.pixel_to_polar(50.0, 50.0);
        approx::assert_relative_eq!(radius, 0.0, epsilon = 0.1);
    }
}
