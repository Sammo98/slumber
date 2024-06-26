use crate::{
    collection::{Authentication, ProfileId, Recipe, RecipeId},
    http::RecipeOptions,
    tui::{
        context::TuiContext,
        input::Action,
        message::{Message, MessageSender, RequestConfig},
        view::{
            common::{
                actions::ActionsModal,
                table::{Table, ToggleRow},
                tabs::Tabs,
                template_preview::TemplatePreview,
                text_window::TextWindow,
                Pane,
            },
            draw::{Draw, Generate, ToStringGenerate},
            event::{Event, EventHandler, EventQueue, Update},
            state::{
                persistence::{Persistable, Persistent, PersistentKey},
                select::SelectState,
                StateCell,
            },
            Component,
        },
    },
};
use derive_more::Display;
use itertools::Itertools;
use ratatui::{
    layout::Layout,
    prelude::{Constraint, Rect},
    widgets::{Paragraph, Row, TableState},
    Frame,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use strum::{EnumCount, EnumIter};

/// Display a request recipe
#[derive(Debug)]
pub struct RecipePane {
    tabs: Component<Tabs<Tab>>,
    /// Needed for template preview rendering
    messages_tx: MessageSender,
    /// All UI state derived from the recipe is stored together, and reset when
    /// the recipe or profile changes
    recipe_state: StateCell<RecipeStateKey, RecipeState>,
}

impl RecipePane {
    pub fn new(messages_tx: MessageSender) -> Self {
        Self {
            tabs: Tabs::new(PersistentKey::RecipeTab).into(),
            messages_tx,
            recipe_state: Default::default(),
        }
    }
}

pub struct RecipePaneProps<'a> {
    pub is_selected: bool,
    pub selected_recipe: Option<&'a Recipe>,
    pub selected_profile_id: Option<&'a ProfileId>,
}

/// Template preview state will be recalculated when any of these fields change
#[derive(Debug, PartialEq)]
struct RecipeStateKey {
    selected_profile_id: Option<ProfileId>,
    recipe_id: RecipeId,
}

#[derive(Debug)]
struct RecipeState {
    url: TemplatePreview,
    query: Component<Persistent<SelectState<RowState, TableState>>>,
    headers: Component<Persistent<SelectState<RowState, TableState>>>,
    body: Option<Component<TextWindow<TemplatePreview>>>,
    authentication: Option<Component<AuthenticationDisplay>>,
}

#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    Display,
    EnumCount,
    EnumIter,
    PartialEq,
    Serialize,
    Deserialize,
)]
enum Tab {
    #[default]
    Body,
    Query,
    Headers,
    Authentication,
}

/// One row in the query/header table
#[derive(Debug)]
struct RowState {
    key: String,
    value: TemplatePreview,
    enabled: Persistent<bool>,
}

/// Items in the actions popup menu
#[derive(Copy, Clone, Debug, Display, EnumCount, EnumIter, PartialEq)]
#[allow(clippy::enum_variant_names)]
enum MenuAction {
    #[display("Copy URL")]
    CopyUrl,
    // TODO disable this if request doesn't have body
    #[display("Copy Body")]
    CopyBody,
    #[display("Copy as cURL")]
    CopyCurl,
}

impl ToStringGenerate for MenuAction {}

impl RecipePane {
    /// Generate a [RecipeOptions] instance based on current UI state
    pub fn recipe_options(&self) -> RecipeOptions {
        if let Some(state) = self.recipe_state.get() {
            /// Convert select state into the set of disabled keys
            fn to_disabled_set(
                select_state: &SelectState<RowState, TableState>,
            ) -> HashSet<String> {
                select_state
                    .items()
                    .iter()
                    .filter(|row| !*row.enabled)
                    .map(|row| row.key.clone())
                    .collect()
            }

            RecipeOptions {
                disabled_headers: to_disabled_set(&state.headers),
                disabled_query_parameters: to_disabled_set(&state.query),
            }
        } else {
            // Shouldn't be possible, because state is initialized on first
            // render
            RecipeOptions::default()
        }
    }

    fn handle_menu_action(
        &mut self,
        messages_tx: &MessageSender,
        action: MenuAction,
    ) {
        // Should always be initialized after first render
        let key = self
            .recipe_state
            .key()
            .expect("Request state not initialized");
        let request_config = RequestConfig {
            profile_id: key.selected_profile_id.clone(),
            recipe_id: key.recipe_id.clone(),
            options: self.recipe_options(),
        };
        let message = match action {
            MenuAction::CopyUrl => Message::CopyRequestUrl(request_config),
            MenuAction::CopyBody => Message::CopyRequestBody(request_config),
            MenuAction::CopyCurl => Message::CopyRequestCurl(request_config),
        };
        messages_tx.send(message);
    }
}

impl EventHandler for RecipePane {
    fn update(&mut self, messages_tx: &MessageSender, event: Event) -> Update {
        match &event {
            Event::Input {
                action: Some(Action::OpenActions),
                ..
            } => EventQueue::open_modal_default::<ActionsModal<MenuAction>>(),
            Event::Other(callback) => {
                match callback.downcast_ref::<MenuAction>() {
                    Some(action) => {
                        self.handle_menu_action(messages_tx, *action);
                    }
                    None => return Update::Propagate(event),
                }
            }
            _ => return Update::Propagate(event),
        }
        Update::Consumed
    }

    fn children(&mut self) -> Vec<Component<&mut dyn EventHandler>> {
        let selected_tab = *self.tabs.selected();
        let mut children = vec![self.tabs.as_child()];

        // Send events to the tab pane as well
        if let Some(state) = self.recipe_state.get_mut() {
            match selected_tab {
                Tab::Body => {
                    if let Some(body) = state.body.as_mut() {
                        children.push(body.as_child());
                    }
                }
                Tab::Query => children.push(state.query.as_child()),
                Tab::Headers => children.push(state.headers.as_child()),
                Tab::Authentication => {}
            }
        }

        children
    }
}

impl<'a> Draw<RecipePaneProps<'a>> for RecipePane {
    fn draw(&self, frame: &mut Frame, props: RecipePaneProps<'a>, area: Rect) {
        // Render outermost block
        let title = TuiContext::get()
            .input_engine
            .add_hint("Recipe", Action::SelectRecipe);
        let block = Pane {
            title: &title,
            is_focused: props.is_selected,
        };
        let block = block.generate();
        let inner_area = block.inner(area);
        frame.render_widget(block, area);

        // Render request contents
        if let Some(recipe) = props.selected_recipe {
            let method = recipe.method.to_string();

            let [metadata_area, tabs_area, content_area] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .areas(inner_area);

            let [method_area, url_area] = Layout::horizontal(
                // Method gets just as much as it needs, URL gets the rest
                [Constraint::Max(method.len() as u16 + 1), Constraint::Min(0)],
            )
            .areas(metadata_area);

            // Whenever the recipe or profile changes, generate a preview for
            // each templated value. Almost anything that could change the
            // preview will either involve changing one of those two things, or
            // would require reloading the whole collection which will reset
            // UI state.
            let recipe_state = self.recipe_state.get_or_update(
                RecipeStateKey {
                    selected_profile_id: props.selected_profile_id.cloned(),
                    recipe_id: recipe.id.clone(),
                },
                || {
                    RecipeState::new(
                        &self.messages_tx,
                        recipe,
                        props.selected_profile_id,
                    )
                },
            );

            // First line: Method + URL
            frame.render_widget(Paragraph::new(method), method_area);
            frame.render_widget(&recipe_state.url, url_area);

            // Navigation tabs
            self.tabs.draw(frame, (), tabs_area);

            // Request content
            match self.tabs.selected() {
                Tab::Body => {
                    if let Some(body) = &recipe_state.body {
                        body.draw(frame, (), content_area);
                    }
                }
                Tab::Query => frame.render_stateful_widget(
                    to_table(&recipe_state.query, ["", "Parameter", "Value"])
                        .generate(),
                    content_area,
                    &mut recipe_state.query.state_mut(),
                ),
                Tab::Headers => frame.render_stateful_widget(
                    to_table(&recipe_state.headers, ["", "Header", "Value"])
                        .generate(),
                    content_area,
                    &mut recipe_state.headers.state_mut(),
                ),
                Tab::Authentication => {
                    if let Some(authentication) = &recipe_state.authentication {
                        authentication.draw(frame, (), content_area)
                    }
                }
            }
        }
    }
}

impl RecipeState {
    /// Initialize new recipe state. Should be called whenever the recipe or
    /// profile changes
    fn new(
        messages_tx: &MessageSender,
        recipe: &Recipe,
        selected_profile_id: Option<&ProfileId>,
    ) -> Self {
        let query_items = recipe
            .query
            .iter()
            .map(|(param, value)| {
                RowState::new(
                    param.clone(),
                    TemplatePreview::new(
                        messages_tx,
                        value.clone(),
                        selected_profile_id.cloned(),
                    ),
                    PersistentKey::RecipeQuery {
                        recipe: recipe.id.clone(),
                        param: param.clone(),
                    },
                )
            })
            .collect();
        let header_items = recipe
            .headers
            .iter()
            .map(|(header, value)| {
                RowState::new(
                    header.clone(),
                    TemplatePreview::new(
                        messages_tx,
                        value.clone(),
                        selected_profile_id.cloned(),
                    ),
                    PersistentKey::RecipeHeader {
                        recipe: recipe.id.clone(),
                        header: header.clone(),
                    },
                )
            })
            .collect();

        Self {
            url: TemplatePreview::new(
                messages_tx,
                recipe.url.clone(),
                selected_profile_id.cloned(),
            ),
            query: Persistent::new(
                PersistentKey::RecipeSelectedQuery(recipe.id.clone()),
                SelectState::builder(query_items)
                    .on_submit(RowState::on_submit)
                    .build(),
            )
            .into(),
            headers: Persistent::new(
                PersistentKey::RecipeSelectedHeader(recipe.id.clone()),
                SelectState::builder(header_items)
                    .on_submit(RowState::on_submit)
                    .build(),
            )
            .into(),
            body: recipe.body.as_ref().map(|body| {
                TextWindow::new(TemplatePreview::new(
                    messages_tx,
                    body.clone(),
                    selected_profile_id.cloned(),
                ))
                .into()
            }),
            // Map authentication type
            authentication: recipe.authentication.as_ref().map(
                |authentication| {
                    match authentication {
                        Authentication::Basic { username, password } => {
                            AuthenticationDisplay::Basic {
                                username: TemplatePreview::new(
                                    messages_tx,
                                    username.clone(),
                                    selected_profile_id.cloned(),
                                ),
                                password: password.clone().map(|password| {
                                    TemplatePreview::new(
                                        messages_tx,
                                        password,
                                        selected_profile_id.cloned(),
                                    )
                                }),
                            }
                        }
                        Authentication::Bearer(token) => {
                            AuthenticationDisplay::Bearer(TemplatePreview::new(
                                messages_tx,
                                token.clone(),
                                selected_profile_id.cloned(),
                            ))
                        }
                    }
                    .into() // Convert to Component
                },
            ),
        }
    }
}

/// Display authentication settings. This is basically the underlying
/// [Authentication] type, but the templates have been rendered
#[derive(Debug)]
enum AuthenticationDisplay {
    Basic {
        username: TemplatePreview,
        password: Option<TemplatePreview>,
    },
    Bearer(TemplatePreview),
}

impl Draw for AuthenticationDisplay {
    fn draw(&self, frame: &mut Frame, _: (), area: Rect) {
        match self {
            AuthenticationDisplay::Basic { username, password } => {
                let table = Table {
                    rows: vec![
                        ["Type".into(), "Bearer".into()],
                        ["Username".into(), username.generate()],
                        [
                            "Password".into(),
                            password
                                .as_ref()
                                .map(Generate::generate)
                                .unwrap_or_default(),
                        ],
                    ],
                    column_widths: &[Constraint::Length(8), Constraint::Min(0)],
                    ..Default::default()
                };
                frame.render_widget(table.generate(), area)
            }
            AuthenticationDisplay::Bearer(token) => {
                let table = Table {
                    rows: vec![
                        ["Type".into(), "Bearer".into()],
                        ["Token".into(), token.generate()],
                    ],
                    column_widths: &[Constraint::Length(5), Constraint::Min(0)],
                    ..Default::default()
                };
                frame.render_widget(table.generate(), area)
            }
        }
    }
}

impl RowState {
    fn new(
        key: String,
        value: TemplatePreview,
        persistent_key: PersistentKey,
    ) -> Self {
        Self {
            key,
            value,
            enabled: Persistent::new(
                persistent_key,
                // Value itself is the container, so just pass a default value
                true,
            ),
        }
    }

    /// Toggle row state on submit
    fn on_submit(row: &mut Self) {
        *row.enabled ^= true;
    }
}

/// Convert table select state into a renderable table
fn to_table<'a>(
    state: &'a SelectState<RowState, TableState>,
    header: [&'a str; 3],
) -> Table<'a, 3, Row<'a>> {
    Table {
        rows: state
            .items()
            .iter()
            .map(|item| {
                ToggleRow::new(
                    [item.key.as_str().into(), item.value.generate()],
                    *item.enabled,
                )
                .generate()
            })
            .collect_vec(),
        header: Some(header),
        column_widths: &[
            Constraint::Min(3),
            Constraint::Percentage(50),
            Constraint::Percentage(50),
        ],
        ..Default::default()
    }
}

/// This impl persists just which row is *selected*
impl Persistable for RowState {
    type Persisted = String;

    fn get_persistent(&self) -> &Self::Persisted {
        &self.key
    }
}

impl PartialEq<RowState> for String {
    fn eq(&self, other: &RowState) -> bool {
        self == &other.key
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::*;
    use factori::create;
    use ratatui::{backend::TestBackend, Terminal};
    use rstest::rstest;

    /// Create component to be tested
    #[rstest::fixture]
    fn component(
        _tui_context: (),
        mut messages: MessageQueue,
        mut terminal: Terminal<TestBackend>,
    ) -> RecipePane {
        let recipe = create!(Recipe, url: "https://test/".into());
        let component = RecipePane::new(messages.tx().clone());

        // Draw once to initialize state
        component.draw(
            &mut terminal.get_frame(),
            RecipePaneProps {
                is_selected: true,
                selected_recipe: Some(&recipe),
                selected_profile_id: None,
            },
            Rect::default(),
        );
        // Clear template preview messages so we can test what we want
        messages.clear();
        component
    }

    /// Test "Copy URL" action
    #[rstest]
    fn test_copy_url(mut component: RecipePane, mut messages: MessageQueue) {
        let update = component
            .update(messages.tx(), Event::new_other(MenuAction::CopyUrl));
        // unstable: https://github.com/rust-lang/rust/issues/82775
        assert!(matches!(update, Update::Consumed));

        let message = messages.pop_now();
        let Message::CopyRequestUrl(request_config) = &message else {
            panic!("Wrong message: {message:?}")
        };
        assert_eq!(
            request_config,
            &RequestConfig {
                recipe_id: "recipe1".into(),
                profile_id: None,
                options: RecipeOptions::default()
            }
        );
    }

    /// Test "Copy Body" action
    #[rstest]
    fn test_copy_body(mut component: RecipePane, mut messages: MessageQueue) {
        let update = component
            .update(messages.tx(), Event::new_other(MenuAction::CopyBody));
        // unstable: https://github.com/rust-lang/rust/issues/82775
        assert!(matches!(update, Update::Consumed));

        let message = messages.pop_now();
        let Message::CopyRequestBody(request_config) = &message else {
            panic!("Wrong message: {message:?}")
        };
        assert_eq!(
            request_config,
            &RequestConfig {
                recipe_id: "recipe1".into(),
                profile_id: None,
                options: RecipeOptions::default()
            }
        );
    }

    /// Test "Copy as cURL" action
    #[rstest]
    fn test_copy_as_curl(
        mut component: RecipePane,
        mut messages: MessageQueue,
    ) {
        let update = component
            .update(messages.tx(), Event::new_other(MenuAction::CopyCurl));
        // unstable: https://github.com/rust-lang/rust/issues/82775
        assert!(matches!(update, Update::Consumed));

        let message = messages.pop_now();
        let Message::CopyRequestCurl(request_config) = &message else {
            panic!("Wrong message: {message:?}")
        };
        assert_eq!(
            request_config,
            &RequestConfig {
                recipe_id: "recipe1".into(),
                profile_id: None,
                options: RecipeOptions::default()
            }
        );
    }
}
