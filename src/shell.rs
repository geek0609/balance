//! The application frame: navigation, page header, status bar, the Go To
//! palette, keyboard shortcuts, notifications and the dashboard.

use crate::theme::{self, Palette, Tone};
use crate::{App, Page};
use balance::inventory::InvoiceKind;
use balance::{DayBookEntry, VoucherKind};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Key, Layout, Margin, Modifiers, RichText,
    Sense, Stroke, Theme, Vec2,
};
use std::time::{Duration, Instant};

pub(crate) const VERSION: &str = match option_env!("BALANCE_UPDATE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

pub(crate) struct NavItem {
    pub page: Page,
    pub icon: &'static str,
    pub label: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub keywords: &'static str,
}

pub(crate) struct NavSection {
    pub title: &'static str,
    pub items: &'static [NavItem],
}

const fn nav(
    page: Page,
    icon: &'static str,
    label: &'static str,
    title: &'static str,
    description: &'static str,
    keywords: &'static str,
) -> NavItem {
    NavItem {
        page,
        icon,
        label,
        title,
        description,
        keywords,
    }
}

pub(crate) const NAV: &[NavSection] = &[
    NavSection {
        title: "Overview",
        items: &[nav(
            Page::Home,
            "🏠",
            "Dashboard",
            "Dashboard",
            "Key figures, recent activity and quick actions",
            "home gateway overview summary",
        )],
    },
    NavSection {
        title: "Masters",
        items: &[
            nav(
                Page::Company,
                "🏢",
                "Companies",
                "Companies",
                "Create and select companies, and keep their details and period lock",
                "company profile period lock cutoff financial year select switch",
            ),
            nav(
                Page::Ledger,
                "📒",
                "Ledgers & groups",
                "Ledgers",
                "Chart of accounts, opening balances, party details and payment terms",
                "ledger account group chart opening balance party credit terms debtor creditor",
            ),
            nav(
                Page::Currencies,
                "💱",
                "Currencies",
                "Currencies",
                "Base currency, foreign currencies and exchange rates",
                "forex exchange rate foreign",
            ),
            nav(
                Page::CostCentres,
                "🎯",
                "Cost centres",
                "Cost centres",
                "Track income and expenses by department, project or site",
                "cost centre center allocation",
            ),
            nav(
                Page::Budgets,
                "📊",
                "Budgets",
                "Budgets",
                "Plan amounts by ledger and compare them with actuals",
                "budget variance plan",
            ),
        ],
    },
    NavSection {
        title: "Transactions",
        items: &[
            nav(
                Page::Voucher,
                "✏",
                "Vouchers",
                "Voucher entry",
                "Journal, payment, receipt, contra, sales and purchase vouchers",
                "voucher journal payment receipt contra transfer entry posting",
            ),
            nav(
                Page::Invoice,
                "📃",
                "Invoices",
                "Invoices",
                "Bill customers, record supplier invoices and settle them",
                "invoice bill sales purchase settlement credit debit note",
            ),
            nav(
                Page::Orders,
                "📋",
                "Orders & quotations",
                "Sales and purchase orders",
                "Quotations, sales orders and purchase orders",
                "order quotation quote so po",
            ),
            nav(
                Page::Advances,
                "💳",
                "Advances",
                "Customer and supplier advances",
                "Money received or paid before an invoice exists",
                "advance deposit prepayment",
            ),
            nav(
                Page::Recurring,
                "🔁",
                "Monthly journals",
                "Monthly journals",
                "Journals that repeat every month",
                "recurring repeat schedule standing",
            ),
            nav(
                Page::Payroll,
                "👥",
                "Payroll",
                "Payroll",
                "Employees, pay heads, accruals and salary payments",
                "salary employee pay wages",
            ),
        ],
    },
    NavSection {
        title: "Inventory",
        items: &[
            nav(
                Page::Stock,
                "📦",
                "Stock items",
                "Stock",
                "Items, units, groups, locations, batches, prices and movements",
                "stock item unit godown location batch price list reorder transfer adjustment",
            ),
            nav(
                Page::StockNotes,
                "🚚",
                "Delivery & receipt notes",
                "Delivery and receipt notes",
                "Goods sent to customers and received from suppliers",
                "delivery note receipt note challan grn dispatch",
            ),
            nav(
                Page::StockCounts,
                "📑",
                "Stock counts",
                "Physical stock counts",
                "Count stock on hand and post the differences",
                "physical count stocktake verification",
            ),
            nav(
                Page::StockJournals,
                "â‡„",
                "Stock journals",
                "Stock journals",
                "Post and review warehouse movements, losses and production",
                "stock journal warehouse transfer consumption production by-products import",
            ),
            nav(
                Page::Manufacturing,
                "🏭",
                "Manufacturing",
                "Manufacturing",
                "Bills of materials and production runs",
                "bom production assembly",
            ),
            nav(
                Page::Valuation,
                "💹",
                "Stock valuation",
                "Stock valuation",
                "Inventory value and stock ageing",
                "fifo average cost valuation closing stock ageing aging",
            ),
        ],
    },
    NavSection {
        title: "Banking",
        items: &[nav(
            Page::Bank,
            "🏦",
            "Bank reconciliation",
            "Bank clearance",
            "Clear bank entries and match imported statements",
            "bank reconciliation brs clearance statement csv",
        )],
    },
    NavSection {
        title: "Reports",
        items: &[
            nav(
                Page::Reports,
                "📈",
                "Financial statements",
                "Financial reports",
                "Trial balance, profit and loss and balance sheet",
                "trial balance profit loss p&l balance sheet",
            ),
            nav(
                Page::DayBook,
                "📖",
                "Day book",
                "Day book",
                "Every voucher in a date range",
                "daybook vouchers list",
            ),
            nav(
                Page::Register,
                "📜",
                "Ledger activity",
                "Ledger activity",
                "Postings and running balance for a ledger",
                "ledger statement account register vouchers",
            ),
            nav(
                Page::Outstanding,
                "⏳",
                "Outstanding",
                "Outstanding invoices",
                "Receivables and payables with ageing",
                "receivable payable ageing bills due overdue",
            ),
            nav(
                Page::InvoiceRegister,
                "📁",
                "Sales & purchase register",
                "Sales and purchase register",
                "Invoices by date, party and kind",
                "sales register purchase register invoice list",
            ),
            nav(
                Page::TaxRegister,
                "🏷",
                "Tax register",
                "Invoice tax register",
                "Tax charged and paid on invoices",
                "gst vat tax",
            ),
            nav(
                Page::CashFlow,
                "💰",
                "Cash flow",
                "Cash flow",
                "Cash movement by operating, investing and financing activity",
                "cash flow statement",
            ),
        ],
    },
    NavSection {
        title: "Data",
        items: &[
            nav(
                Page::Import,
                "📥",
                "Import XML",
                "Import accounting XML",
                "Bring masters and vouchers in from an accounting XML export",
                "import xml migrate",
            ),
            nav(
                Page::Backup,
                "💾",
                "Back up & restore",
                "Back up data",
                "Snapshot the database or restore a backup into a new file",
                "backup restore snapshot",
            ),
        ],
    },
];

pub(crate) fn nav_item(page: Page) -> (&'static NavSection, &'static NavItem) {
    NAV.iter()
        .find_map(|section| {
            section
                .items
                .iter()
                .find(|item| item.page == page)
                .map(|item| (section, item))
        })
        .expect("every page has a navigation entry")
}

fn needs_company(page: Page) -> bool {
    !matches!(page, Page::Home | Page::Company | Page::Backup)
}

#[derive(Clone, Copy)]
enum Action {
    Go(Page),
    Voucher(VoucherKind),
    Invoice(InvoiceKind),
    Company(i64),
    ToggleTheme,
}

/// Function-key shortcuts for transaction entry.
const SHORTCUTS: &[(&str, &str, Action)] = &[
    ("F4", "Contra", Action::Voucher(VoucherKind::Transfer)),
    ("F5", "Payment", Action::Voucher(VoucherKind::Payment)),
    ("F6", "Receipt", Action::Voucher(VoucherKind::Receipt)),
    ("F7", "Journal", Action::Voucher(VoucherKind::Journal)),
    ("F8", "Sales", Action::Invoice(InvoiceKind::Sale)),
    ("F9", "Purchase", Action::Invoice(InvoiceKind::Purchase)),
];

struct GoTo {
    query: String,
    selected: usize,
    focus: bool,
}

struct Entry {
    icon: &'static str,
    label: String,
    detail: String,
    keys: &'static str,
    haystack: String,
    action: Action,
}

#[derive(Default)]
pub(crate) struct Shell {
    go_to: Option<GoTo>,
    dashboard: Option<Dashboard>,
    message_seen: String,
    message_since: Option<Instant>,
    pub last_failure: String,
}

impl Shell {
    pub(crate) fn go_to_is_open(&self) -> bool {
        self.go_to.is_some()
    }
}

struct Dashboard {
    company: i64,
    loaded: Instant,
    data: Result<DashboardData, String>,
}

struct DashboardData {
    today: String,
    currency: Option<String>,
    income: i128,
    expenses: i128,
    profit: i128,
    assets: i128,
    liabilities: i128,
    equity: i128,
    receivable: i128,
    receivable_count: usize,
    overdue: i128,
    overdue_count: usize,
    payable: i128,
    payable_count: usize,
    low_stock: usize,
    voucher_count: usize,
    recent: Vec<DayBookEntry>,
}

fn today() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|d| d.date_naive().format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// Infer how to present a status message. Screens report successes and errors
/// through the same field; failures routed through App::failure are exact.
fn classify(message: &str, last_failure: &str) -> Tone {
    if message == last_failure {
        return Tone::Danger;
    }
    let lower = message.to_lowercase();
    const PROBLEMS: &[&str] = &[
        "cannot",
        "can't",
        "could not",
        "couldn't",
        "failed",
        "invalid",
        "must",
        "not ",
        "unable",
        "error",
        "missing",
        "required",
        "requires",
        "exceed",
        "unknown",
        "denied",
    ];
    const SUCCESSES: &[&str] = &[
        "saved",
        "created",
        "posted",
        "exported",
        "recorded",
        "added",
        "updated",
        "removed",
        "matched",
        "restored",
        "calculated",
        "filled",
        "prepared",
        "loaded",
        "released",
        "reversed",
        "cleared",
        "closed",
        "imported",
        "skipped",
        "converted",
        "applied",
        "deleted",
        "generated",
        "copied",
    ];
    let problem = PROBLEMS.iter().any(|p| lower.contains(p)) || lower.starts_with("no ");
    let success = lower
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| SUCCESSES.contains(&word));
    match (success, problem) {
        (true, false) => Tone::Success,
        (true, true) => Tone::Warning,
        _ if lower.starts_with("select ")
            || lower.starts_with("enter ")
            || lower.starts_with("choose ")
            || lower.starts_with("save or discard") =>
        {
            Tone::Warning
        }
        _ => Tone::Danger,
    }
}

fn initials(name: &str) -> String {
    let mut letters: String = name
        .split_whitespace()
        .filter_map(|w| w.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .collect();
    if letters.is_empty() {
        letters.push('?');
    }
    letters.to_uppercase()
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
        cut.push('…');
        cut
    }
}

impl App {
    pub(crate) fn shell_update(&mut self, ctx: &egui::Context) {
        let p = theme::palette(ctx);
        self.handle_shortcuts(ctx);
        self.sidebar(ctx, &p);
        self.status_bar(ctx, &p);
        self.page_header(ctx, &p);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg))
            .show(ctx, |ui| {
                self.message_banner(ui, &p);
                egui::ScrollArea::vertical()
                    .id_salt(("page", self.page))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui::Frame::new()
                            .inner_margin(Margin {
                                left: 24,
                                right: 24,
                                top: 18,
                                bottom: 32,
                            })
                            .show(ui, |ui| self.page_body(ui));
                    });
            });
        self.go_to_palette(ctx, &p);
    }

    pub(crate) fn navigate(&mut self, page: Page) {
        if self.page != page {
            self.page = page;
            self.message.clear();
        }
        if page == Page::Home {
            self.shell.dashboard = None;
        }
    }

    pub(crate) fn switch_company(&mut self, company: i64) {
        if self.company == Some(company) {
            return;
        }
        if self.has_unsaved_edits() {
            self.message = "Save or discard the current edits before changing company.".into();
            return;
        }
        self.company = Some(company);
        self.reset_company_forms();
        self.message.clear();
        self.shell.dashboard = None;
    }

    fn open_voucher(&mut self, kind: VoucherKind) {
        self.navigate(Page::Voucher);
        if self.voucher_kind != kind {
            if self.editing_voucher.is_some()
                || !self.voucher_reference.trim().is_empty()
                || !self.narration.trim().is_empty()
                || self
                    .lines
                    .iter()
                    .any(|line| line.ledger.is_some() || !line.amount.trim().is_empty())
            {
                self.message =
                    "Finish the current voucher before starting one of a different type.".into();
            } else {
                self.voucher_kind = kind;
            }
        }
    }

    fn open_invoice(&mut self, kind: InvoiceKind) {
        self.navigate(Page::Invoice);
        if self.invoice_kind != kind {
            if self.invoice_draft_in_use()
                || !self.invoice_reference.trim().is_empty()
                || !self.invoice_bill_reference.trim().is_empty()
                || !self.invoice_bill_splits.is_empty()
                || !self.invoice_narration.trim().is_empty()
                || !self.invoice_tax_amount.trim().is_empty()
                || !self.invoice_additional_taxes.is_empty()
                || !self.invoice_adjustments.is_empty()
                || self.invoice_lines.iter().any(|line| {
                    line.item.is_some()
                        || !line.quantity.trim().is_empty()
                        || !line.billed_quantity.trim().is_empty()
                        || !line.price.trim().is_empty()
                })
            {
                self.message =
                    "Finish the current invoice before starting one of a different type.".into();
            } else {
                self.invoice_kind = kind;
            }
        }
    }

    fn run(&mut self, ctx: &egui::Context, action: Action) {
        let company_needed = !matches!(action, Action::Company(_) | Action::ToggleTheme)
            && !matches!(action, Action::Go(page) if !needs_company(page));
        if company_needed && self.company.is_none() {
            self.navigate(Page::Home);
            self.message = "Select a company first.".into();
            return;
        }
        match action {
            Action::Go(page) => self.navigate(page),
            Action::Voucher(kind) => self.open_voucher(kind),
            Action::Invoice(kind) => self.open_invoice(kind),
            Action::Company(id) => {
                self.switch_company(id);
                if self.company == Some(id) {
                    self.navigate(Page::Home);
                }
            }
            Action::ToggleTheme => {
                let next = match ctx.theme() {
                    Theme::Dark => Theme::Light,
                    Theme::Light => Theme::Dark,
                };
                ctx.set_theme(next);
            }
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if self.shell.go_to.is_some() {
            return;
        }
        let (open, action) = ctx.input_mut(|i| {
            let open = i.consume_key(Modifiers::COMMAND, Key::K)
                || i.consume_key(Modifiers::COMMAND, Key::G)
                || i.consume_key(Modifiers::ALT, Key::G);
            let action = if i.consume_key(Modifiers::COMMAND, Key::F8) {
                Some(Action::Voucher(VoucherKind::CreditNote))
            } else if i.consume_key(Modifiers::COMMAND, Key::F9) {
                Some(Action::Voucher(VoucherKind::DebitNote))
            } else if i.consume_key(Modifiers::COMMAND, Key::H) {
                Some(Action::Go(Page::Home))
            } else {
                [Key::F4, Key::F5, Key::F6, Key::F7, Key::F8, Key::F9]
                    .into_iter()
                    .zip(SHORTCUTS)
                    .find(|(key, _)| i.consume_key(Modifiers::NONE, *key))
                    .map(|(_, (_, _, action))| *action)
            };
            (open, action)
        });
        if open {
            self.shell.go_to = Some(GoTo {
                query: String::new(),
                selected: 0,
                focus: true,
            });
        }
        if let Some(action) = action {
            self.run(ctx, action);
        }
    }

    fn sidebar(&mut self, ctx: &egui::Context, p: &Palette) {
        egui::SidePanel::left("navigation")
            .exact_width(252.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(p.sidebar_bg).inner_margin(Margin {
                left: 12,
                right: 12,
                top: 16,
                bottom: 10,
            }))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                self.brand(ui, p);
                ui.add_space(14.0);
                self.company_switcher(ui, p);
                ui.add_space(8.0);
                if self.go_to_button(ui, p) {
                    self.shell.go_to = Some(GoTo {
                        query: String::new(),
                        selected: 0,
                        focus: true,
                    });
                }
                ui.add_space(6.0);
                let footer = 40.0;
                let mut target = None;
                egui::ScrollArea::vertical()
                    .id_salt("navigation-scroll")
                    .max_height((ui.available_height() - footer).max(80.0))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for section in NAV {
                            ui.add_space(12.0);
                            ui.horizontal(|ui| {
                                ui.add_space(10.0);
                                ui.label(theme::caption(section.title, p.sidebar_muted));
                            });
                            ui.add_space(2.0);
                            for item in section.items {
                                let dim = needs_company(item.page) && self.company.is_none();
                                if nav_row(
                                    ui,
                                    p,
                                    item.icon,
                                    item.label,
                                    self.page == item.page,
                                    dim,
                                )
                                .clicked()
                                {
                                    target = Some(item.page);
                                }
                            }
                        }
                        ui.add_space(8.0);
                    });
                if let Some(page) = target {
                    self.run(ctx, Action::Go(page));
                }
                self.sidebar_footer(ui, p);
            });
    }

    fn brand(&self, ui: &mut egui::Ui, p: &Palette) {
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::hover());
            let painter = ui.painter();
            painter.rect_filled(rect, CornerRadius::same(theme::CORNER), p.accent);
            // Two balanced bars: a small mark for "Balance".
            let c = rect.center();
            for dy in [-4.0, 4.0] {
                painter.rect_filled(
                    egui::Rect::from_center_size(c + Vec2::new(0.0, dy), Vec2::new(14.0, 3.0)),
                    CornerRadius::same(2),
                    p.on_accent,
                );
            }
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(
                    RichText::new("Balance")
                        .font(theme::semibold(17.0))
                        .color(p.sidebar_text),
                );
                ui.label(
                    RichText::new("Accounting & inventory")
                        .size(11.0)
                        .color(p.sidebar_muted),
                );
            });
        });
    }

    fn company_switcher(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let companies = self.store.companies().unwrap_or_default();
        let current = self
            .company
            .and_then(|id| companies.iter().find(|c| c.id == id))
            .map(|c| c.name.clone());
        let width = ui.available_width();
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 52.0), Sense::click());
        let hovered = response.hovered();
        let painter = ui.painter();
        painter.rect_filled(
            rect,
            CornerRadius::same(theme::CORNER),
            if hovered {
                p.sidebar_hover
            } else {
                p.sidebar_surface
            },
        );
        theme::paint_outline(
            ui,
            rect,
            if response.has_focus() {
                p.sidebar_text
            } else {
                p.sidebar_border
            },
            theme::CORNER,
        );
        let avatar = egui::Pos2::new(rect.left() + 26.0, rect.center().y);
        let name = current.as_deref().unwrap_or("Choose a company");
        painter.circle_filled(
            avatar,
            15.0,
            if current.is_some() {
                p.accent
            } else {
                p.sidebar_hover
            },
        );
        painter.text(
            avatar,
            Align2::CENTER_CENTER,
            if current.is_some() {
                initials(name)
            } else {
                "＋".into()
            },
            theme::semibold(12.0),
            if current.is_some() {
                p.on_accent
            } else {
                p.sidebar_text
            },
        );
        painter.text(
            egui::Pos2::new(rect.left() + 50.0, rect.top() + 15.0),
            Align2::LEFT_CENTER,
            "COMPANY",
            theme::semibold(9.5),
            p.sidebar_muted,
        );
        painter.text(
            egui::Pos2::new(rect.left() + 50.0, rect.top() + 34.0),
            Align2::LEFT_CENTER,
            truncate(name, 20),
            theme::semibold(14.0),
            p.sidebar_text,
        );
        painter.text(
            egui::Pos2::new(rect.right() - 14.0, rect.center().y),
            Align2::RIGHT_CENTER,
            "⬇",
            FontId::proportional(11.0),
            p.sidebar_muted,
        );
        let response = response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("Switch company");
        let mut chosen = None;
        let mut manage = false;
        egui::Popup::menu(&response).width(width).show(|ui| {
            ui.set_min_width(width - 12.0);
            if companies.is_empty() {
                ui.label(RichText::new("No companies yet").weak());
            }
            for company in &companies {
                let selected = self.company == Some(company.id);
                if ui
                    .selectable_label(
                        selected,
                        format!("{}   {}", initials(&company.name), company.name),
                    )
                    .clicked()
                {
                    chosen = Some(company.id);
                }
            }
            ui.separator();
            if ui.button("⚙  Manage companies…").clicked() {
                manage = true;
            }
        });
        if let Some(id) = chosen {
            self.switch_company(id);
        }
        if manage {
            self.navigate(Page::Company);
        }
    }

    fn go_to_button(&self, ui: &mut egui::Ui, p: &Palette) -> bool {
        let width = ui.available_width();
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 34.0), Sense::click());
        let painter = ui.painter();
        painter.rect_filled(
            rect,
            CornerRadius::same(theme::CORNER),
            if response.hovered() {
                p.sidebar_hover
            } else {
                p.sidebar_surface
            },
        );
        theme::paint_outline(
            ui,
            rect,
            if response.has_focus() {
                p.sidebar_text
            } else {
                p.sidebar_border
            },
            theme::CORNER,
        );
        painter.text(
            rect.left_center() + Vec2::new(12.0, 0.0),
            Align2::LEFT_CENTER,
            "🔍  Go to…",
            FontId::proportional(13.0),
            p.sidebar_muted,
        );
        let keys = egui::Rect::from_center_size(
            egui::Pos2::new(rect.right() - 30.0, rect.center().y),
            Vec2::new(44.0, 20.0),
        );
        painter.rect_filled(keys, CornerRadius::same(theme::CORNER), p.sidebar_bg);
        painter.text(
            keys.center(),
            Align2::CENTER_CENTER,
            "Ctrl K",
            FontId::proportional(11.0),
            p.sidebar_muted,
        );
        response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("Jump to any screen or action (Ctrl+K or Alt+G)")
            .clicked()
    }

    fn sidebar_footer(&self, ui: &mut egui::Ui, p: &Palette) {
        let rect = ui.available_rect_before_wrap();
        ui.painter().hline(
            rect.x_range(),
            rect.top(),
            Stroke::new(1.0, p.sidebar_border),
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!("v{VERSION}"))
                    .size(11.0)
                    .color(p.sidebar_muted),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let file = self
                    .database_path
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default();
                ui.label(
                    RichText::new(format!("🗄 {}", truncate(&file, 22)))
                        .size(11.0)
                        .color(p.sidebar_muted),
                )
                .on_hover_text(format!("Database: {}", self.database_path.display()));
            });
        });
    }

    fn status_bar(&mut self, ctx: &egui::Context, p: &Palette) {
        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::new()
                    .fill(p.surface_alt)
                    .inner_margin(Margin::symmetric(16, 5)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 5.0;
                    theme::kbd(ui, "Ctrl K", false);
                    ui.label(RichText::new("Go to").size(11.5).color(p.text_muted));
                    for (keys, label, _) in SHORTCUTS {
                        ui.add_space(6.0);
                        theme::kbd(ui, keys, false);
                        ui.label(RichText::new(*label).size(11.5).color(p.text_muted));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(self.database_path.display().to_string())
                                .size(11.0)
                                .color(p.text_faint),
                        )
                        .on_hover_text("Database file");
                    });
                });
            });
    }

    fn page_header(&mut self, ctx: &egui::Context, p: &Palette) {
        let (section, item) = nav_item(self.page);
        egui::TopBottomPanel::top("page-header")
            .frame(
                egui::Frame::new()
                    .fill(p.surface_alt)
                    .inner_margin(Margin::symmetric(24, 12)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            ui.label(RichText::new(section.title).size(12.0).color(p.text_faint));
                            ui.label(RichText::new("›").size(12.0).color(p.text_faint));
                            ui.label(RichText::new(item.label).size(12.0).color(p.text_muted));
                        });
                        ui.label(
                            RichText::new(item.title)
                                .font(theme::semibold(22.0))
                                .color(p.text),
                        );
                        ui.label(RichText::new(item.description).size(12.5).color(p.text_muted));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let dark = ctx.theme() == Theme::Dark;
                        if ui
                            .add(
                                egui::Button::new(RichText::new(if dark { "☀" } else { "🌙" }).size(15.0))
                                    .min_size(Vec2::splat(32.0)),
                            )
                            .on_hover_text(if dark {
                                "Switch to light theme"
                            } else {
                                "Switch to dark theme"
                            })
                            .clicked()
                        {
                            self.run(ctx, Action::ToggleTheme);
                        }
                        #[cfg(all(windows, target_arch = "x86_64", feature = "windows-updater"))]
                        self.updates.show(ui);
                        if let Some(company) = self.company {
                            if let Ok(Some(date)) = self.store.period_lock(company) {
                                theme::badge(ui, &format!("🔒 Locked through {date}"), Tone::Warning)
                                    .on_hover_text(
                                        "Transactions dated on or before this day are locked. Change it on the Companies page.",
                                    );
                            }
                            if let Ok(Some(code)) = self.store.company_base_currency(company) {
                                theme::badge(ui, &code, Tone::Neutral).on_hover_text("Base currency");
                            }
                        }
                    });
                });
            });
    }

    fn message_banner(&mut self, ui: &mut egui::Ui, p: &Palette) {
        if self.message.is_empty() {
            self.shell.message_since = None;
            return;
        }
        if self.shell.message_seen != self.message || self.shell.message_since.is_none() {
            self.shell.message_seen = self.message.clone();
            self.shell.message_since = Some(Instant::now());
        }
        let tone = classify(&self.message, &self.shell.last_failure);
        if tone == Tone::Success {
            const SHOW_FOR: Duration = Duration::from_secs(8);
            let age = self
                .shell
                .message_since
                .map(|t| t.elapsed())
                .unwrap_or_default();
            if age >= SHOW_FOR {
                self.message.clear();
                return;
            }
            ui.ctx().request_repaint_after(SHOW_FOR - age);
        }
        let (fg, bg) = tone.colors(p);
        let icon = match tone {
            Tone::Success => "✔",
            Tone::Danger | Tone::Warning => "⚠",
            _ => "ℹ",
        };
        let mut dismiss = false;
        egui::Frame::new()
            .fill(bg)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(CornerRadius::same(theme::CORNER))
            .inner_margin(Margin::symmetric(14, 9))
            .outer_margin(Margin {
                left: 24,
                right: 24,
                top: 14,
                bottom: 0,
            })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon).size(15.0).color(fg));
                    let width = (ui.available_width() - 36.0).max(80.0);
                    ui.allocate_ui_with_layout(
                        Vec2::new(width, 20.0),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.add(
                                egui::Label::new(RichText::new(&self.message).color(p.text)).wrap(),
                            );
                        },
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        dismiss = theme::ghost_button(ui, RichText::new("✖").size(12.0).color(fg))
                            .on_hover_text("Dismiss")
                            .clicked();
                    });
                });
            });
        if dismiss {
            self.message.clear();
        }
    }

    fn page_body(&mut self, ui: &mut egui::Ui) {
        let (_, item) = nav_item(self.page);
        theme::set_page_title(ui.ctx(), item.title);
        match self.page {
            Page::Home => return self.dashboard(ui),
            Page::Company => return theme::card(ui, |ui| self.company_page(ui)),
            Page::Backup => return theme::card(ui, |ui| self.backup_page(ui)),
            _ => {}
        }
        let Some(company) = self.company else {
            return self.choose_company_first(ui);
        };
        theme::card(ui, |ui| match self.page {
            Page::Home | Page::Company | Page::Backup => {}
            Page::Ledger => self.ledger_page(ui, company),
            Page::Voucher => self.voucher_page(ui, company),
            Page::Recurring => self.show_recurring_journals(ui, company),
            Page::Currencies => self.currency_page(ui, company),
            Page::Register => self.register_page(ui, company),
            Page::DayBook => self.day_book_page(ui, company),
            Page::Outstanding => self.outstanding_page(ui, company),
            Page::Advances => self.advances_page(ui, company),
            Page::InvoiceRegister => self.invoice_register_page(ui, company),
            Page::TaxRegister => self.tax_register_page(ui, company),
            Page::Bank => self.bank_page(ui, company),
            Page::CashFlow => self.cash_flow_page(ui, company),
            Page::Stock => self.stock_page(ui, company),
            Page::StockCounts => self.stock_counts_page(ui, company),
            Page::StockJournals => self.stock_journals_page(ui, company),
            Page::StockNotes => self.stock_notes_page(ui, company),
            Page::Valuation => self.valuation_page(ui, company),
            Page::Manufacturing => self.manufacturing_page(ui, company),
            Page::Invoice => self.invoice_page(ui, company),
            Page::Orders => self.orders_page(ui, company),
            Page::Import => self.import_page(ui, company),
            Page::Reports => self.reports_page(ui, company),
            Page::CostCentres => self.cost_page(ui, company),
            Page::Budgets => self.budgets_page(ui, company),
            Page::Payroll => self.payroll_page(ui, company),
        });
    }

    fn choose_company_first(&mut self, ui: &mut egui::Ui) {
        let p = theme::ui_palette(ui);
        let mut go = false;
        theme::card(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(24.0);
                ui.label(RichText::new("🏢").size(36.0).color(p.text_faint));
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Choose a company first")
                        .font(theme::semibold(18.0))
                        .color(p.text),
                );
                ui.label(
                    RichText::new(
                        "This screen works on one company's books. Pick the company to continue.",
                    )
                    .color(p.text_muted),
                );
                ui.add_space(10.0);
                go = theme::primary_button(ui, "Choose a company").clicked();
                ui.add_space(24.0);
            });
        });
        if go {
            self.navigate(Page::Home);
        }
    }

    fn go_to_entries(&self, ctx: &egui::Context) -> Vec<Entry> {
        let mut entries = Vec::new();
        let mut push = |icon, label: String, detail: String, keys, keywords: &str, action| {
            let haystack = format!("{label} {detail} {keywords}").to_lowercase();
            entries.push(Entry {
                icon,
                label,
                detail,
                keys,
                haystack,
                action,
            });
        };
        for (keys, label, action) in SHORTCUTS {
            let (icon, text) = match action {
                Action::Invoice(_) => ("📃", format!("New {} invoice", label.to_lowercase())),
                _ => ("✏", format!("New {} voucher", label.to_lowercase())),
            };
            push(
                icon,
                text,
                "Create".into(),
                *keys,
                "new create add entry",
                *action,
            );
        }
        push(
            "✏",
            "New credit note".into(),
            "Create".into(),
            "Ctrl F8",
            "voucher new create",
            Action::Voucher(VoucherKind::CreditNote),
        );
        push(
            "✏",
            "New debit note".into(),
            "Create".into(),
            "Ctrl F9",
            "voucher new create",
            Action::Voucher(VoucherKind::DebitNote),
        );
        for section in NAV {
            for item in section.items {
                push(
                    item.icon,
                    item.title.into(),
                    section.title.into(),
                    if item.page == Page::Home {
                        "Ctrl H"
                    } else {
                        ""
                    },
                    &format!("{} {} {}", item.label, item.description, item.keywords),
                    Action::Go(item.page),
                );
            }
        }
        for company in self.store.companies().unwrap_or_default() {
            push(
                "🏢",
                format!("Switch to {}", company.name),
                "Company".into(),
                "",
                "company open select",
                Action::Company(company.id),
            );
        }
        let dark = ctx.theme() == Theme::Dark;
        push(
            if dark { "☀" } else { "🌙" },
            if dark {
                "Use light theme"
            } else {
                "Use dark theme"
            }
            .into(),
            "Appearance".into(),
            "",
            "theme dark light mode appearance colour color",
            Action::ToggleTheme,
        );
        entries
    }

    fn go_to_palette(&mut self, ctx: &egui::Context, p: &Palette) {
        let Some(mut state) = self.shell.go_to.take() else {
            return;
        };
        let entries = self.go_to_entries(ctx);
        let query = state.query.trim().to_lowercase();
        let mut matches: Vec<(usize, &Entry)> = entries
            .iter()
            .filter(|e| {
                query
                    .split_whitespace()
                    .all(|word| e.haystack.contains(word))
            })
            .map(|e| {
                let label = e.label.to_lowercase();
                let rank = if query.is_empty() {
                    1
                } else if label.starts_with(&query) {
                    0
                } else if label.contains(&query) {
                    1
                } else {
                    2
                };
                (rank, e)
            })
            .collect();
        matches.sort_by_key(|(rank, _)| *rank);
        let (down, up, enter, escape) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if !matches.is_empty() {
            if down {
                state.selected = (state.selected + 1) % matches.len();
            }
            if up {
                state.selected = (state.selected + matches.len() - 1) % matches.len();
            }
            state.selected = state.selected.min(matches.len() - 1);
        }
        let mut chosen = if enter && !matches.is_empty() {
            Some(state.selected)
        } else {
            None
        };
        let id = egui::Id::new("go-to");
        let modal = egui::Modal::new(id)
            .backdrop_color(Color32::from_black_alpha(110))
            .area(egui::Modal::default_area(id).anchor(Align2::CENTER_TOP, Vec2::new(0.0, 90.0)))
            .frame(
                egui::Frame::new()
                    .fill(p.surface)
                    .stroke(Stroke::new(1.0, p.border_strong))
                    .corner_radius(CornerRadius::same(theme::CORNER))
                    .inner_margin(Margin::same(10)),
            )
            .show(ctx, |ui| {
                ui.set_width(600.0);
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    ui.label(RichText::new("🔍").size(16.0).color(p.text_faint));
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut state.query)
                            .hint_text("Search screens, reports and actions…")
                            .font(FontId::proportional(17.0))
                            .frame(false)
                            .desired_width(f32::INFINITY)
                            .margin(Margin::symmetric(4, 8)),
                    );
                    if state.focus {
                        edit.request_focus();
                        state.focus = false;
                    }
                    if edit.changed() {
                        state.selected = 0;
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(400.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        if matches.is_empty() {
                            ui.add_space(16.0);
                            ui.vertical_centered(|ui| {
                                ui.label(RichText::new("No matches").color(p.text_muted));
                            });
                            ui.add_space(16.0);
                        }
                        for (index, (_, entry)) in matches.iter().enumerate() {
                            let selected = index == state.selected;
                            let row = palette_row(ui, p, entry, selected);
                            if selected && (up || down) {
                                row.scroll_to_me(None);
                            }
                            if row.clicked() {
                                chosen = Some(index);
                            }
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.add_space(6.0);
                    for (keys, label) in [("↑↓", "move"), ("Enter", "open"), ("Esc", "close")] {
                        theme::kbd(ui, keys, false);
                        ui.label(RichText::new(label).size(11.5).color(p.text_muted));
                        ui.add_space(8.0);
                    }
                });
            });
        let action = chosen
            .and_then(|index| matches.get(index))
            .map(|(_, e)| e.action);
        if action.is_none() && !escape && !modal.should_close() {
            self.shell.go_to = Some(state);
        }
        if let Some(action) = action {
            self.run(ctx, action);
        }
    }

    fn dashboard(&mut self, ui: &mut egui::Ui) {
        let Some(company) = self.company else {
            return self.welcome(ui);
        };
        let stale =
            self.shell.dashboard.as_ref().is_none_or(|d| {
                d.company != company || d.loaded.elapsed() > Duration::from_secs(30)
            });
        if stale {
            self.shell.dashboard = Some(Dashboard {
                company,
                loaded: Instant::now(),
                data: self.load_dashboard(company),
            });
        }
        let Some(dashboard) = self.shell.dashboard.take() else {
            return;
        };
        let mut action = None;
        let mut refresh = false;
        match &dashboard.data {
            Ok(data) => self.draw_dashboard(ui, data, &mut action, &mut refresh),
            Err(error) => {
                theme::card(ui, |ui| {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        format!("Could not load the dashboard: {error}"),
                    );
                    refresh = ui.button("Try again").clicked();
                });
            }
        }
        if !refresh {
            self.shell.dashboard = Some(dashboard);
        }
        if let Some(action) = action {
            self.run(ui.ctx(), action);
        }
    }

    fn load_dashboard(&self, company: i64) -> Result<DashboardData, String> {
        let today = today();
        let report = self
            .store
            .financial_report(company, None, Some(&today))
            .map_err(|e| e.to_string())?;
        let outstanding = self
            .store
            .outstanding_invoices(company, &today)
            .map_err(|e| e.to_string())?;
        let low_stock = self
            .store
            .low_stock(company, Some(&today))
            .map_err(|e| e.to_string())?
            .len();
        let vouchers = self
            .store
            .day_book(company, None, Some(&today), None, "")
            .map_err(|e| e.to_string())?;
        let mut data = DashboardData {
            today,
            currency: self.store.company_base_currency(company).ok().flatten(),
            income: report.income,
            expenses: report.expenses,
            profit: report.profit,
            assets: report.assets,
            liabilities: report.liabilities,
            equity: report.equity,
            receivable: 0,
            receivable_count: 0,
            overdue: 0,
            overdue_count: 0,
            payable: 0,
            payable_count: 0,
            low_stock,
            voucher_count: vouchers.len(),
            recent: vouchers.into_iter().take(8).collect(),
        };
        for invoice in outstanding.iter().filter(|i| i.outstanding != 0) {
            match invoice.kind {
                InvoiceKind::Sale => {
                    data.receivable += invoice.outstanding;
                    data.receivable_count += 1;
                    if invoice.overdue_days.is_some_and(|d| d > 0) {
                        data.overdue += invoice.outstanding;
                        data.overdue_count += 1;
                    }
                }
                InvoiceKind::Purchase => {
                    data.payable += invoice.outstanding;
                    data.payable_count += 1;
                }
            }
        }
        Ok(data)
    }

    fn draw_dashboard(
        &self,
        ui: &mut egui::Ui,
        d: &DashboardData,
        action: &mut Option<Action>,
        refresh: &mut bool,
    ) {
        let p = theme::ui_palette(ui);
        let currency = d.currency.as_deref();
        let symbol = theme::currency_symbol(currency);
        let money = |value: i128| format!("{symbol}{}", theme::amount(value, currency));
        let company = self
            .company
            .and_then(|id| {
                self.store
                    .companies()
                    .ok()?
                    .into_iter()
                    .find(|c| c.id == id)
            })
            .map(|c| c.name)
            .unwrap_or_default();

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(
                    RichText::new(&company)
                        .font(theme::semibold(20.0))
                        .color(p.text),
                );
                ui.label(
                    RichText::new(format!(
                        "Through {} (UTC) · {} posted vouchers",
                        d.today, d.voucher_count
                    ))
                    .color(p.text_muted),
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("↻  Refresh").clicked() {
                    *refresh = true;
                }
            });
        });
        ui.add_space(12.0);

        let tiles = [
            (
                "Income",
                money(d.income),
                "Cumulative income".to_owned(),
                Tone::Success,
                Page::Reports,
            ),
            (
                "Expenses",
                money(d.expenses),
                "Cumulative expenses".to_owned(),
                Tone::Warning,
                Page::Reports,
            ),
            (
                "Net profit",
                money(d.profit),
                if d.profit >= 0 {
                    "Income less expenses".to_owned()
                } else {
                    "Net loss: expenses exceed income".to_owned()
                },
                if d.profit >= 0 {
                    Tone::Success
                } else {
                    Tone::Danger
                },
                Page::Reports,
            ),
            (
                "Receivables",
                money(d.receivable),
                if d.overdue_count > 0 {
                    format!(
                        "{} open · {} overdue ({})",
                        d.receivable_count,
                        d.overdue_count,
                        money(d.overdue)
                    )
                } else {
                    format!("{} open invoices · none overdue", d.receivable_count)
                },
                if d.overdue_count > 0 {
                    Tone::Danger
                } else {
                    Tone::Info
                },
                Page::Outstanding,
            ),
            (
                "Payables",
                money(d.payable),
                format!("{} open supplier invoices", d.payable_count),
                Tone::Accent,
                Page::Outstanding,
            ),
            (
                "Low stock",
                d.low_stock.to_string(),
                if d.low_stock == 1 {
                    "item at or below its reorder level".to_owned()
                } else {
                    "items at or below their reorder level".to_owned()
                },
                if d.low_stock > 0 {
                    Tone::Warning
                } else {
                    Tone::Neutral
                },
                Page::Stock,
            ),
        ];
        let gap = 14.0;
        let available = ui.available_width();
        let columns = (((available + gap) / (230.0 + gap)).floor() as usize).clamp(1, 6);
        let columns = match columns {
            4 | 5 => 3,
            n => n,
        };
        let width = (available - gap * (columns as f32 - 1.0)) / columns as f32;
        for row in tiles.chunks(columns) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for (label, value, note, tone, page) in row {
                    let response = theme::stat_tile(ui, width, label, value, note, *tone)
                        .interact(Sense::click())
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    if response.hovered() || response.has_focus() {
                        theme::paint_outline(ui, response.rect, p.accent, theme::CORNER);
                    }
                    if response.clicked() {
                        *action = Some(Action::Go(*page));
                    }
                }
            });
            ui.add_space(gap - ui.spacing().item_spacing.y);
        }

        let wide = available > 900.0;
        let left_width = if wide {
            (available - gap) * 0.6
        } else {
            available
        };
        let right_width = if wide {
            available - gap - left_width
        } else {
            available
        };
        let picked = std::cell::Cell::new(None);
        let mut recent = |ui: &mut egui::Ui| {
            ui.set_width(left_width);
            theme::card(ui, |ui| {
                ui.horizontal(|ui| {
                    theme::card_title(ui, "Recent vouchers", Some("The latest entries by date"));
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        if theme::ghost_button(ui, RichText::new("Open day book →").color(p.accent))
                            .clicked()
                        {
                            picked.set(Some(Action::Go(Page::DayBook)));
                        }
                    });
                });
                if d.recent.is_empty() {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("No vouchers yet. Press F5, F6 or F7 to record one.")
                            .color(p.text_muted),
                    );
                    ui.add_space(8.0);
                    return;
                }
                egui::Grid::new("dashboard-recent")
                    .num_columns(4)
                    .striped(true)
                    .spacing(Vec2::new(14.0, 8.0))
                    .min_col_width(0.0)
                    .show(ui, |ui| {
                        for heading in ["Date", "Type", "Particulars"] {
                            ui.label(theme::caption(heading, p.text_faint));
                        }
                        let amount_width = 130.0;
                        ui.allocate_ui_with_layout(
                            Vec2::new(amount_width, 20.0),
                            Layout::right_to_left(Align::Center),
                            |ui| {
                                ui.label(theme::caption("Amount", p.text_faint));
                            },
                        );
                        ui.end_row();
                        for entry in &d.recent {
                            let voucher = &entry.voucher;
                            ui.label(RichText::new(&voucher.date).color(p.text_muted));
                            theme::badge(ui, voucher.kind.as_str(), voucher_tone(voucher.kind));
                            let particulars =
                                match (voucher.reference.trim(), voucher.narration.trim()) {
                                    ("", "") => format!("Voucher #{}", voucher.id),
                                    ("", narration) => narration.to_owned(),
                                    (reference, "") => reference.to_owned(),
                                    (reference, narration) => format!("{reference} · {narration}"),
                                };
                            ui.label(truncate(&particulars, 46))
                                .on_hover_text(format!("Voucher #{}\n{particulars}", voucher.id));
                            ui.allocate_ui_with_layout(
                                Vec2::new(amount_width, 20.0),
                                Layout::right_to_left(Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(money(entry.debit_total))
                                            .font(theme::semibold(13.5))
                                            .color(p.text),
                                    );
                                },
                            );
                            ui.end_row();
                        }
                    });
            });
        };
        let mut side = |ui: &mut egui::Ui| {
            ui.set_width(right_width);
            theme::card(ui, |ui| {
                theme::card_title(
                    ui,
                    "Quick actions",
                    Some("Function keys work from any screen"),
                );
                let button_width = (ui.available_width() - 8.0) / 2.0;
                let quick = [
                    ("✏", "Payment", "F5", Action::Voucher(VoucherKind::Payment)),
                    ("✏", "Receipt", "F6", Action::Voucher(VoucherKind::Receipt)),
                    ("✏", "Journal", "F7", Action::Voucher(VoucherKind::Journal)),
                    ("✏", "Contra", "F4", Action::Voucher(VoucherKind::Transfer)),
                    (
                        "📃",
                        "Sales invoice",
                        "F8",
                        Action::Invoice(InvoiceKind::Sale),
                    ),
                    (
                        "📃",
                        "Purchase invoice",
                        "F9",
                        Action::Invoice(InvoiceKind::Purchase),
                    ),
                    ("📒", "Ledgers", "", Action::Go(Page::Ledger)),
                    ("📈", "Financial reports", "", Action::Go(Page::Reports)),
                ];
                for pair in quick.chunks(2) {
                    ui.horizontal(|ui| {
                        for (icon, label, keys, target) in pair {
                            let mut button = egui::Button::new(format!("{icon}  {label}"))
                                .min_size(Vec2::new(button_width, 34.0));
                            if !keys.is_empty() {
                                button = button.right_text(
                                    RichText::new(*keys).size(11.0).color(p.text_faint),
                                );
                            }
                            if ui.add(button).clicked() {
                                picked.set(Some(*target));
                            }
                        }
                    });
                }
            });
            ui.add_space(gap);
            theme::card(ui, |ui| {
                theme::card_title(ui, "Financial position", None);
                egui::Grid::new("dashboard-position")
                    .num_columns(2)
                    .spacing(Vec2::new(14.0, 8.0))
                    .show(ui, |ui| {
                        for (label, value) in [
                            ("Assets", d.assets),
                            ("Liabilities", d.liabilities),
                            ("Equity incl. profit", d.equity),
                        ] {
                            ui.label(RichText::new(label).color(p.text_muted));
                            ui.label(
                                RichText::new(money(value))
                                    .font(theme::semibold(14.0))
                                    .color(p.text),
                            );
                            ui.end_row();
                        }
                    });
                let difference = d.assets - d.liabilities - d.equity;
                ui.add_space(4.0);
                if difference == 0 {
                    theme::badge(ui, "✔ Balance sheet agrees", Tone::Success);
                } else {
                    theme::badge(ui, &format!("⚠ Out by {}", money(difference)), Tone::Danger);
                }
            });
        };
        if wide {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                ui.vertical(&mut recent);
                ui.vertical(&mut side);
            });
        } else {
            recent(ui);
            ui.add_space(gap);
            side(ui);
        }
        if let Some(picked) = picked.get() {
            *action = Some(picked);
        }
    }

    fn welcome(&mut self, ui: &mut egui::Ui) {
        let p = theme::ui_palette(ui);
        let companies = self.store.companies().unwrap_or_default();
        let mut chosen = None;
        let mut create = false;
        let width = ui.available_width().min(640.0);
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.set_max_width(width);
            theme::card(ui, |ui| {
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Welcome to Balance")
                        .font(theme::semibold(26.0))
                        .color(p.text),
                );
                ui.label(
                    RichText::new(if companies.is_empty() {
                        "Create your first company to start keeping books."
                    } else {
                        "Pick the company you want to work on."
                    })
                    .color(p.text_muted),
                );
                ui.add_space(14.0);
                for company in &companies {
                    let (rect, response) = ui
                        .allocate_exact_size(Vec2::new(ui.available_width(), 52.0), Sense::click());
                    let painter = ui.painter();
                    painter.rect_filled(
                        rect,
                        CornerRadius::same(theme::CORNER),
                        if response.hovered() {
                            p.hover
                        } else {
                            p.surface_alt
                        },
                    );
                    theme::paint_outline(
                        ui,
                        rect,
                        if response.hovered() {
                            p.accent
                        } else {
                            p.border
                        },
                        theme::CORNER,
                    );
                    let avatar = egui::Pos2::new(rect.left() + 28.0, rect.center().y);
                    painter.circle_filled(avatar, 16.0, p.accent);
                    painter.text(
                        avatar,
                        Align2::CENTER_CENTER,
                        initials(&company.name),
                        theme::semibold(12.5),
                        p.on_accent,
                    );
                    painter.text(
                        egui::Pos2::new(rect.left() + 56.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        &company.name,
                        theme::semibold(15.0),
                        p.text,
                    );
                    painter.text(
                        egui::Pos2::new(rect.right() - 16.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        "Open →",
                        FontId::proportional(13.0),
                        if response.hovered() {
                            p.accent
                        } else {
                            p.text_faint
                        },
                    );
                    if response
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        chosen = Some(company.id);
                    }
                    ui.add_space(2.0);
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    create = theme::primary_button(
                        ui,
                        if companies.is_empty() {
                            "＋  Create your first company"
                        } else {
                            "＋  New company"
                        },
                    )
                    .clicked();
                });
                ui.add_space(4.0);
            });
            ui.add_space(14.0);
            ui.label(
                RichText::new("Tip: press Ctrl+K anywhere to jump to a screen.")
                    .size(12.0)
                    .color(p.text_faint),
            );
        });
        if let Some(id) = chosen {
            self.switch_company(id);
        }
        if create {
            self.navigate(Page::Company);
        }
    }
}

fn voucher_tone(kind: VoucherKind) -> Tone {
    match kind {
        VoucherKind::Payment | VoucherKind::Purchase | VoucherKind::DebitNote => Tone::Warning,
        VoucherKind::Receipt | VoucherKind::Sales | VoucherKind::CreditNote => Tone::Success,
        VoucherKind::Transfer => Tone::Info,
        VoucherKind::Journal => Tone::Neutral,
    }
}

fn nav_row(
    ui: &mut egui::Ui,
    p: &Palette,
    icon: &str,
    label: &str,
    selected: bool,
    dim: bool,
) -> egui::Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 32.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label)
    });
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, CornerRadius::same(theme::CORNER), p.selection);
    } else if response.hovered() {
        painter.rect_filled(rect, CornerRadius::same(theme::CORNER), p.sidebar_hover);
    }
    let text = if selected {
        p.on_selection
    } else if dim {
        p.sidebar_muted
    } else {
        p.sidebar_text
    };
    painter.text(
        rect.left_center() + Vec2::new(22.0, 0.0),
        Align2::CENTER_CENTER,
        icon,
        FontId::proportional(14.0),
        if selected {
            p.on_selection
        } else {
            p.sidebar_muted
        },
    );
    painter.text(
        rect.left_center() + Vec2::new(40.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        if selected {
            theme::semibold(13.5)
        } else {
            FontId::proportional(13.5)
        },
        text,
    );
    if response.has_focus() {
        theme::paint_outline(ui, rect, text, theme::CORNER);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn palette_row(ui: &mut egui::Ui, p: &Palette, entry: &Entry, selected: bool) -> egui::Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 38.0), Sense::click());
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, CornerRadius::same(theme::CORNER), p.selection);
    } else if response.hovered() {
        painter.rect_filled(rect, CornerRadius::same(theme::CORNER), p.hover);
    }
    painter.text(
        rect.left_center() + Vec2::new(20.0, 0.0),
        Align2::CENTER_CENTER,
        entry.icon,
        FontId::proportional(15.0),
        if selected {
            p.on_selection
        } else {
            p.text_muted
        },
    );
    painter.text(
        rect.left_center() + Vec2::new(40.0, 0.0),
        Align2::LEFT_CENTER,
        &entry.label,
        if selected {
            theme::semibold(14.0)
        } else {
            FontId::proportional(14.0)
        },
        if selected { p.on_selection } else { p.text },
    );
    let mut right = rect.right() - 12.0;
    if !entry.keys.is_empty() {
        let galley = painter.layout_no_wrap(
            entry.keys.to_owned(),
            FontId::proportional(11.0),
            p.text_muted,
        );
        let keys = egui::Rect::from_min_size(
            egui::Pos2::new(right - galley.size().x - 10.0, rect.center().y - 10.0),
            Vec2::new(galley.size().x + 10.0, 20.0),
        );
        painter.rect_filled(keys, CornerRadius::same(4), p.surface_alt);
        theme::paint_outline(ui, keys, p.border_strong, 4);
        painter.galley(
            keys.min + Vec2::new(5.0, (20.0 - galley.size().y) / 2.0),
            galley,
            p.text_muted,
        );
        right = keys.left() - 10.0;
    }
    painter.text(
        egui::Pos2::new(right, rect.center().y),
        Align2::RIGHT_CENTER,
        &entry.detail,
        FontId::proportional(12.0),
        if selected {
            p.on_selection
        } else {
            p.text_faint
        },
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[cfg(test)]
mod tests {
    use super::{classify, NAV};
    use crate::theme::Tone;
    use crate::App;
    use balance::{Category, Posting, Store};
    use eframe::egui;

    #[test]
    fn classifies_status_messages() {
        assert!(classify("Voucher 4 saved", "") == Tone::Success);
        assert!(
            classify(
                "Opening bill 3 posted. Find it under Invoices or Outstanding.",
                ""
            ) == Tone::Success
        );
        assert!(classify("No opening balance recorded for this ledger", "") == Tone::Warning);
        assert!(classify("Select a stock item", "") == Tone::Warning);
        assert!(classify("Saved. Could not refresh the register: busy", "") == Tone::Warning);
        assert!(classify("The start date must not be after the end date", "") == Tone::Danger);
        assert!(classify("Invoice 2 saved", "Invoice 2 saved") == Tone::Danger);
    }

    #[test]
    fn every_page_is_listed_once() {
        let mut pages: Vec<_> = NAV
            .iter()
            .flat_map(|s| s.items.iter().map(|i| i.page))
            .collect();
        let total = pages.len();
        pages.sort_by_key(|p| format!("{p:?}"));
        pages.dedup();
        assert_eq!(pages.len(), total);
        assert_eq!(total, 28);
    }

    #[test]
    fn dashboard_renders_with_a_consistent_cutoff() {
        let mut store = Store::memory().unwrap();
        let company = store.create_company("Dashboard demo").unwrap();
        let cash = store
            .create_ledger(company, "Cash", Category::Asset)
            .unwrap();
        let income = store
            .create_ledger(company, "Services", Category::Income)
            .unwrap();
        for (date, amount) in [("2000-01-01", 1000), ("9999-12-31", 9000)] {
            store
                .post_voucher(
                    company,
                    date,
                    "Example",
                    &[
                        Posting {
                            ledger_id: cash,
                            amount,
                        },
                        Posting {
                            ledger_id: income,
                            amount: -amount,
                        },
                    ],
                )
                .unwrap();
        }
        let mut app = App::new(store, "demo.db".into());
        app.switch_company(company);
        for mode in [egui::Theme::Light, egui::Theme::Dark] {
            let context = egui::Context::default();
            crate::theme::install(&context);
            context.set_theme(mode);
            let frame = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1360.0, 860.0),
                    )),
                    ..Default::default()
                },
                |context| app.shell_update(context),
            );
            assert_eq!(context.theme(), mode);
            assert!(!frame.shapes.is_empty());
        }
        let dashboard = app.shell.dashboard.as_ref().unwrap().data.as_ref().unwrap();
        assert_eq!(dashboard.voucher_count, 1);
        assert_eq!(dashboard.recent[0].voucher.date, "2000-01-01");
        assert_eq!(dashboard.income, 1000);
        assert_eq!(dashboard.assets, 1000);
    }
}
