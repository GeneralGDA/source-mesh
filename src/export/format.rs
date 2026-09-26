use clap::ValueEnum;

use super::dot::Dot;
use super::exporter::Exporter;
use super::graphml::GraphMl;
use super::mermaid::Mermaid;

#[derive(Clone, Copy, ValueEnum)]
pub enum Format {
    Dot,
    Mermaid,
    Graphml,
}

impl Format {
    #[must_use]
    pub fn exporter(self) -> Box<dyn Exporter> {
        match self {
            Self::Dot => Box::new(Dot),
            Self::Mermaid => Box::new(Mermaid),
            Self::Graphml => Box::new(GraphMl),
        }
    }
}
