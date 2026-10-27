use super::*;
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum PageTool {
    ListTabs,
    ReadPage,
    ShowItems,
    OpenTabs,
    GoTo,
    Act,
    InspectStyle,
    Restyle,
    HandOff,
}
impl PageTool {
    pub(crate) const ALL: [Self; 9] = [
        Self::ListTabs,
        Self::ReadPage,
        Self::ShowItems,
        Self::OpenTabs,
        Self::GoTo,
        Self::Act,
        Self::InspectStyle,
        Self::Restyle,
        Self::HandOff,
    ];
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::ListTabs => "list_tabs",
            Self::ReadPage => "read_page",
            Self::ShowItems => "show_items",
            Self::OpenTabs => "open_tabs",
            Self::GoTo => "go_to",
            Self::Act => "act",
            Self::InspectStyle => "inspect_style",
            Self::Restyle => "restyle",
            Self::HandOff => "hand_off",
        }
    }
    pub(crate) fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.name() == name)
    }
}
/// Desktop translates only the existing native Page WorkModelTool definitions.
/// There is no renderer-supplied expected digest or arbitrary offered tool map.
pub(crate) struct Definition {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) input_schema: Value,
}
pub(crate) struct NativeManifest {
    wire: Value,
    digest: [u8; 32],
    offered: BTreeSet<PageTool>,
}
impl NativeManifest {
    pub(crate) fn page_v1(definitions: Vec<Definition>) -> Result<Self, Fault> {
        if definitions.len() != PageTool::ALL.len() {
            return Err(Fault::Manifest);
        }
        let mut tools = BTreeMap::new();
        for definition in definitions {
            let tool = PageTool::parse(&definition.name).ok_or(Fault::Manifest)?;
            if definition.description.is_empty()
                || definition.description.len() > 8192
                || !definition.input_schema.is_object()
            {
                return Err(Fault::Manifest);
            }
            bounded_json(&definition.input_schema, 64 * 1024)?;
            let wire = json!({"type":"function","name":tool.name(),"description":definition.description,"inputSchema":definition.input_schema,"deferLoading":false});
            if tools.insert(tool, wire).is_some() {
                return Err(Fault::Manifest);
            }
        }
        if !PageTool::ALL.iter().all(|tool| tools.contains_key(tool)) {
            return Err(Fault::Manifest);
        }
        let offered = tools.keys().copied().collect();
        let wire = Value::Array(tools.into_values().collect());
        bounded_json(&wire, 256 * 1024)?;
        let digest = digest(&json!({"policy":"zephium.page.dynamic-tools.v1","tools":wire}))?;
        Ok(Self {
            wire,
            digest,
            offered,
        })
    }
    pub(crate) fn instruction_digest(instructions: &str) -> Result<[u8; 32], Fault> {
        if instructions.is_empty() || instructions.len() > 128 * 1024 {
            return Err(Fault::Limit);
        }
        digest(&json!({"policy":"zephium.page.instructions.v1","instructions":instructions}))
    }
    pub(crate) fn digest(&self) -> [u8; 32] {
        self.digest
    }
    pub(crate) fn wire(&self) -> &Value {
        &self.wire
    }
    pub(crate) fn contains(&self, tool: PageTool) -> bool {
        self.offered.contains(&tool)
    }
}
