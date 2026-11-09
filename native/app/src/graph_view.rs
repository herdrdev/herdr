//! Draws a pattern graph (positions come from `pattern_graph::layout`).

use crate::prism::{self, Preset};
use crate::theme::{ACCENT, BAD, DIM, HI, LINE2, OK};
use iced::widget::canvas::{self, Path, Stroke, Text};
use iced::{mouse, Color, Point, Rectangle, Size, Theme, Vector};
use shepherd_app_core::pattern_graph::{Graph, Node, NodeKind, NODE_H, NODE_W};

pub struct GraphView<'a> {
    pub graph: &'a Graph,
    /// Key of the selected node, and of the node a connection starts from.
    pub selected: Option<String>,
    pub connecting: Option<String>,
    pub zoom: f32,
    /// Animation step: the dashes of the gradient on the edges run along them.
    pub frame: u32,
    /// The overview map of the inspector: just a box per node.
    pub mini: bool,
}

/// The node under a point of the canvas.
pub fn node_at(graph: &Graph, p: Point) -> Option<&Node> {
    graph
        .nodes
        .iter()
        .rev()
        .find(|n| p.x >= n.x && p.x <= n.x + NODE_W && p.y >= n.y && p.y <= n.y + NODE_H)
}

/// Where an arrow leaves `from` and enters `to`: the facing sides, or bottom to top in the same column.
pub fn anchors(from: &Node, to: &Node) -> (Point, Point) {
    let mid = |n: &Node, side_x: f32| Point::new(n.x + side_x, n.y + NODE_H / 2.0);
    if to.x > from.x + 1.0 {
        (mid(from, NODE_W), mid(to, 0.0))
    } else if to.x + 1.0 < from.x {
        (mid(from, 0.0), mid(to, NODE_W))
    } else if to.y >= from.y {
        (
            Point::new(from.x + NODE_W / 2.0, from.y + NODE_H),
            Point::new(to.x + NODE_W / 2.0, to.y),
        )
    } else {
        (
            Point::new(from.x + NODE_W / 2.0, from.y),
            Point::new(to.x + NODE_W / 2.0, to.y + NODE_H),
        )
    }
}

impl canvas::Program<crate::Message> for GraphView<'_> {
    /// The node being dragged and where inside it the mouse took hold.
    type State = Option<(String, Vector)>;

    fn update(
        &self,
        state: &mut Self::State,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<crate::Message>> {
        use crate::panel::PanelMsg;
        use crate::pattern_editor::EditMsg;
        let publish = |m: EditMsg| {
            Some(
                canvas::Action::publish(crate::Message::Panel(PanelMsg::EditPattern(m)))
                    .and_capture(),
            )
        };
        let iced::Event::Mouse(event) = event else {
            return None;
        };
        match event {
            mouse::Event::ButtonPressed(mouse::Button::Left) => {
                let p = cursor.position_in(bounds)?;
                let graph_point = Point::new(p.x / self.zoom, p.y / self.zoom);
                match node_at(self.graph, graph_point) {
                    Some(node) => {
                        *state = Some((
                            node.key.clone(),
                            Vector::new(graph_point.x - node.x, graph_point.y - node.y),
                        ));
                        publish(EditMsg::Press(node.key.clone()))
                    }
                    None => publish(EditMsg::Press(String::new())),
                }
            }
            mouse::Event::CursorMoved { .. } => {
                let (key, offset) = state.as_ref()?;
                let p = cursor.position_in(bounds)?;
                publish(EditMsg::Move(
                    key.clone(),
                    p.x / self.zoom - offset.x,
                    p.y / self.zoom - offset.y,
                ))
            }
            mouse::Event::ButtonReleased(mouse::Button::Left) => {
                *state = None;
                None
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        frame.scale(self.zoom);
        // `.graph`: #08080C with a dot every 18 px.
        frame.fill_rectangle(
            Point::ORIGIN,
            bounds.size(),
            Color::from_rgb8(0x08, 0x08, 0x0C),
        );
        if self.mini {
            for node in &self.graph.nodes {
                let rect = Path::rounded_rectangle(
                    Point::new(node.x, node.y),
                    Size::new(NODE_W, NODE_H),
                    6.0.into(),
                );
                frame.fill(&rect, Color::from_rgb8(0x15, 0x15, 0x1C));
                let marked = self.selected.as_deref() == Some(node.key.as_str());
                frame.stroke(
                    &rect,
                    Stroke::default()
                        .with_color(if marked { ACCENT } else { LINE2 })
                        .with_width(if marked { 3.0 } else { 2.0 }),
                );
            }
            return vec![frame.into_geometry()];
        }
        let dot = Color::from_rgba8(160, 160, 190, 0.22);
        for x in (0..bounds.width as u32).step_by(18) {
            for y in (0..bounds.height as u32).step_by(18) {
                frame.fill_rectangle(Point::new(x as f32, y as f32), Size::new(1.0, 1.0), dot);
            }
        }
        // Edges leave and enter with a right-angled route, like the mockup; the arrow head follows the last leg.
        for edge in &self.graph.edges {
            let (a, b) = anchors(&self.graph.nodes[edge.from], &self.graph.nodes[edge.to]);
            let (from_node, to_node) = (&self.graph.nodes[edge.from], &self.graph.nodes[edge.to]);
            let horizontal = (to_node.x - from_node.x).abs() > 1.0;
            let route = if (a.x - b.x).abs() < 1.0 || (a.y - b.y).abs() < 1.0 {
                vec![a, b]
            } else if horizontal {
                let mx = (a.x + b.x) / 2.0;
                vec![a, Point::new(mx, a.y), Point::new(mx, b.y), b]
            } else {
                let my = (a.y + b.y) / 2.0;
                vec![a, Point::new(a.x, my), Point::new(b.x, my), b]
            };
            let path = Path::new(|p| {
                p.move_to(route[0]);
                for point in &route[1..] {
                    p.line_to(*point);
                }
            });
            frame.stroke(&path, Stroke::default().with_color(LINE2).with_width(2.0));
            // `.edge`: dashes in the prism gradient that move from the source to the target
            let mut gradient = canvas::gradient::Linear::new(a, b);
            for (stop, (r, g, bl)) in [
                (0.0, (255, 45, 85)),
                (0.33, (255, 212, 0)),
                (0.66, (25, 211, 255)),
                (1.0, (177, 77, 255)),
            ] {
                gradient = gradient.add_stop(stop, Color::from_rgb8(r, g, bl));
            }
            frame.stroke(
                &path,
                Stroke {
                    style: canvas::Style::Gradient(gradient.into()),
                    width: 2.0,
                    line_dash: canvas::LineDash {
                        segments: &[6.0, 8.0],
                        offset: (14 - (self.frame as usize * 5) % 14) % 14,
                    },
                    ..Stroke::default()
                },
            );
            let (from, to) = (route[route.len() - 2], route[route.len() - 1]);
            let (dx, dy) = (to.x - from.x, to.y - from.y);
            let len = (dx * dx + dy * dy).sqrt().max(1.0);
            let (ux, uy) = (dx / len, dy / len);
            let base = Point::new(to.x - ux * 9.0, to.y - uy * 9.0);
            let head = Path::new(|p| {
                p.move_to(to);
                p.line_to(Point::new(base.x - uy * 4.5, base.y + ux * 4.5));
                p.line_to(Point::new(base.x + uy * 4.5, base.y - ux * 4.5));
                p.close();
            });
            frame.fill(&head, DIM);
        }
        for (index, node) in self.graph.nodes.iter().enumerate() {
            let marked = self.selected.as_deref() == Some(node.key.as_str());
            let linking = self.connecting.as_deref() == Some(node.key.as_str());
            let origin = Point::new(node.x, node.y);
            let size = Size::new(NODE_W, NODE_H);
            let rect = Path::rounded_rectangle(origin, size, 6.0.into());
            if marked {
                // glow: `0 0 24px rgba(25,211,255,.18)`
                for (grow, alpha) in [(6.0, 0.05), (3.0, 0.08)] {
                    let halo = Path::rounded_rectangle(
                        Point::new(node.x - grow, node.y - grow),
                        Size::new(NODE_W + 2.0 * grow, NODE_H + 2.0 * grow),
                        (6.0 + grow).into(),
                    );
                    frame.fill(&halo, Color { a: alpha, ..ACCENT });
                }
            }
            frame.fill(&rect, Color::from_rgb8(0x10, 0x10, 0x18));
            let is_agent = matches!(node.kind, NodeKind::Agent);
            let mut stroke = Stroke::default()
                .with_color(if linking {
                    BAD
                } else if marked {
                    ACCENT
                } else if is_agent {
                    Color::from_rgb8(0x3A, 0x3A, 0x4C)
                } else {
                    LINE2
                })
                .with_width(if marked || linking { 1.5 } else { 1.0 });
            if !is_agent {
                stroke.line_dash = canvas::LineDash {
                    segments: &[4.0, 3.0],
                    offset: 0,
                };
            }
            frame.stroke(&rect, stroke);
            if is_agent {
                // `.st`: the animated 3 px prism strip along the top edge, each node a little out of step
                let strip = Size::new(NODE_W - 2.0, 3.0);
                let step = self.frame.wrapping_add(index as u32 * 3);
                for (x, y, [r, g, b]) in prism::cells(strip, &Preset::NODE, step) {
                    frame.fill_rectangle(
                        Point::new(node.x + 1.0 + x as f32, node.y + y as f32),
                        Size::new(1.0, 1.0),
                        Color::from_rgb8(r, g, b),
                    );
                }
            }
            frame.fill_text(Text {
                content: node.label.clone(),
                position: Point::new(node.x + 12.0, node.y + 11.0),
                color: HI,
                size: 14.0.into(),
                ..Text::default()
            });
            frame.fill_text(Text {
                content: node.detail.clone(),
                position: Point::new(node.x + 12.0, node.y + 33.0),
                color: DIM,
                size: 11.0.into(),
                ..Text::default()
            });
            if node.start {
                frame.fill_text(Text {
                    content: "▶".into(),
                    position: Point::new(node.x + NODE_W - 20.0, node.y + 11.0),
                    color: OK,
                    size: 11.0.into(),
                    ..Text::default()
                });
            }
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(x: f32, y: f32) -> Node {
        Node {
            key: String::new(),
            kind: NodeKind::Agent,
            label: String::new(),
            detail: String::new(),
            start: false,
            x,
            y,
        }
    }

    #[test]
    fn a_press_finds_the_node_under_it() {
        let graph = Graph {
            nodes: vec![
                Node {
                    key: "agent:a".into(),
                    ..node(10.0, 10.0)
                },
                Node {
                    key: "topic:t".into(),
                    ..node(300.0, 10.0)
                },
            ],
            edges: vec![],
            width: 500.0,
            height: 100.0,
        };
        assert_eq!(
            node_at(&graph, Point::new(20.0, 20.0)).map(|n| n.key.as_str()),
            Some("agent:a")
        );
        assert_eq!(
            node_at(&graph, Point::new(310.0, 50.0)).map(|n| n.key.as_str()),
            Some("topic:t")
        );
        assert!(node_at(&graph, Point::new(200.0, 20.0)).is_none());
    }

    #[test]
    fn arrows_leave_and_enter_the_facing_sides() {
        let (a, b) = anchors(&node(0.0, 0.0), &node(300.0, 0.0));
        assert_eq!(
            (a.x, b.x),
            (NODE_W, 300.0),
            "forward: right side to left side"
        );
        let (a, b) = anchors(&node(300.0, 0.0), &node(0.0, 0.0));
        assert_eq!(
            (a.x, b.x),
            (300.0, NODE_W),
            "backward: left side to right side"
        );
        let (a, b) = anchors(&node(0.0, 0.0), &node(0.0, 100.0));
        assert_eq!((a.y, b.y), (NODE_H, 100.0), "same column: bottom to top");
    }
}
