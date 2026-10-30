package ltd.kilian.pod.ui

import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.Snackbar
import androidx.compose.material3.SnackbarData
import androidx.compose.material3.Text
import androidx.compose.material3.ripple
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

// ==============================================================================================
// The kit: the few pieces every screen is built from
// ==============================================================================================
//
// Buttons are pills, play buttons circles, panels and rows 12 dp rounded, inputs 8 dp. Material's
// own buttons, chips and list items are tuned for its tonal palettes and would pull the screens
// towards a generic look; these follow the web client's measurements instead.

/** How a [PodButton] reads: the one primary action, an ordinary one, or a quiet text-like one. */
enum class ButtonKind { Primary, Secondary, Quiet }

@Composable
fun PodButton(
    text: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    kind: ButtonKind = ButtonKind.Secondary,
    icon: ImageVector? = null,
    enabled: Boolean = true,
    height: Dp = 40.dp,
    style: TextStyle = PodType.button,
    contentColor: Color? = null,
) {
    val colors = podColors
    val (background, kindContent, border) = when (kind) {
        ButtonKind.Primary -> Triple(colors.accent, colors.onAccent, null)
        ButtonKind.Secondary -> Triple(colors.surface, colors.ink, BorderStroke(1.dp, colors.line))
        ButtonKind.Quiet -> Triple(Color.Transparent, colors.muted, null)
    }
    val content = contentColor ?: kindContent
    Row(
        modifier
            .defaultMinSize(minHeight = height)
            .alpha(if (enabled) 1f else 0.55f)
            .clip(CircleShape)
            .background(background)
            .then(if (border != null) Modifier.border(border, CircleShape) else Modifier)
            .clickable(enabled = enabled, role = Role.Button, onClick = onClick)
            .padding(horizontal = 16.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (icon != null) Icon(icon, contentDescription = null, tint = content, modifier = Modifier.size(18.dp))
        Text(
            text,
            style = if (kind == ButtonKind.Primary) style.copy(fontWeight = androidx.compose.ui.text.font.FontWeight(600)) else style,
            color = content, maxLines = 1, overflow = TextOverflow.Ellipsis,
        )
    }
}

/** A round icon-only button with a 44 dp touch target; the [description] is its name for TalkBack. */
@Composable
fun PodIconButton(
    icon: ImageVector,
    description: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    tint: Color = podColors.muted,
    size: Dp = 44.dp,
    iconSize: Dp = 22.dp,
    enabled: Boolean = true,
) {
    Box(
        modifier
            .size(size)
            .clip(CircleShape)
            .clickable(enabled = enabled, role = Role.Button, onClick = onClick, interactionSource = remember { MutableInteractionSource() }, indication = ripple(bounded = false, radius = size / 2))
            .semantics { contentDescription = description }
            .alpha(if (enabled) 1f else 0.4f),
        contentAlignment = Alignment.Center,
    ) {
        Icon(icon, contentDescription = null, tint = tint, modifier = Modifier.size(iconSize))
    }
}

/** A text link in the accent's ink colour, such as "More about this show". */
@Composable
fun LinkButton(text: String, onClick: () -> Unit, modifier: Modifier = Modifier, style: TextStyle = PodType.metaMedium) {
    Text(
        text, style = style, color = podColors.accentInk,
        modifier = modifier.clip(RoundedCornerShape(6.dp)).clickable(role = Role.Button, onClick = onClick).padding(vertical = 6.dp),
    )
}

/** A small pill stating a fact, such as "✓ Subscribed": not a button, though it may open something. */
@Composable
fun StatePill(text: String, icon: ImageVector?, modifier: Modifier = Modifier, onClick: (() -> Unit)? = null) {
    val colors = podColors
    Row(
        modifier
            .height(34.dp)
            .clip(CircleShape)
            .background(colors.surface.copy(alpha = 0.7f))
            .then(if (onClick != null) Modifier.clickable(onClick = onClick) else Modifier)
            .padding(start = if (icon != null) 10.dp else 14.dp, end = 14.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (icon != null) Icon(icon, contentDescription = null, tint = colors.accent, modifier = Modifier.size(16.dp))
        Text(text, style = PodType.metaMedium, color = colors.muted, maxLines = 1)
    }
}

/** One-of-a-few choices (All / Unplayed) as a segmented control. */
@Composable
fun <T> Segmented(options: List<T>, selected: T, label: (T) -> String, onSelect: (T) -> Unit, modifier: Modifier = Modifier) {
    val colors = podColors
    Row(modifier.clip(CircleShape).background(colors.hover).padding(3.dp)) {
        options.forEach { option ->
            val active = option == selected
            Box(
                Modifier
                    .heightIn(min = 34.dp)
                    .clip(CircleShape)
                    .background(if (active) colors.surface else Color.Transparent)
                    .clickable(role = Role.Tab) { onSelect(option) }
                    .padding(horizontal = 16.dp),
                contentAlignment = Alignment.Center,
            ) {
                Text(label(option), style = PodType.metaMedium, color = if (active) colors.ink else colors.muted)
            }
        }
    }
}

// ==============================================================================================
// Page structure
// ==============================================================================================

/** The horizontal gutter: 16 dp on phones, 28 dp from tablet width up. */
val LocalGutter = androidx.compose.runtime.staticCompositionLocalOf { 16.dp }

/** A page's title row ("Inbox", "Settings"), with room for actions at the end. */
@Composable
fun PageHeader(title: String, modifier: Modifier = Modifier, actions: @Composable RowScope.() -> Unit = {}) {
    Row(
        modifier.fillMaxWidth().padding(start = LocalGutter.current, end = LocalGutter.current - 6.dp, top = 20.dp, bottom = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(title, style = PodType.pageTitle, color = podColors.ink, modifier = Modifier.weight(1f))
        actions()
    }
}

/**
 * The back affordance of a pushed page: an arrow and where it goes ("← Inbox") in the accent's
 * ink, and room for the page's own actions at the end.
 */
@Composable
fun BackBar(label: String?, onBack: () -> Unit, modifier: Modifier = Modifier, actions: @Composable RowScope.() -> Unit = {}) {
    val colors = podColors
    Row(modifier.fillMaxWidth().heightIn(min = 52.dp).padding(start = LocalGutter.current - 8.dp, end = LocalGutter.current - 10.dp), verticalAlignment = Alignment.CenterVertically) {
        Row(
            Modifier
                .weight(1f, fill = false)
                .clip(CircleShape)
                .clickable(role = Role.Button, onClick = onBack)
                .semantics { contentDescription = "Back" + (label?.let { " to $it" } ?: "") }
                .padding(horizontal = 8.dp, vertical = 8.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(PodIcons.ArrowLeft, contentDescription = null, tint = colors.accentInk, modifier = Modifier.size(20.dp))
            if (label != null) Text(label, style = PodType.section, color = colors.accentInk, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        Spacer(Modifier.weight(1f))
        actions()
    }
}

/** A heading over a run of rows ("Today", "In progress"): 13 sp semibold, muted. */
@Composable
fun GroupHeading(text: String, modifier: Modifier = Modifier) {
    Text(
        text, style = PodType.label, color = podColors.muted,
        modifier = modifier.fillMaxWidth().background(podColors.background).padding(start = LocalGutter.current, end = LocalGutter.current, top = 18.dp, bottom = 6.dp),
    )
}

/** A hairline between rows, inset as the web's are. */
@Composable
fun RowDivider(modifier: Modifier = Modifier, inset: Dp = LocalGutter.current) {
    Box(modifier.fillMaxWidth().padding(horizontal = inset).heightIn(min = 1.dp, max = 1.dp).background(podColors.line))
}

/**
 * An empty page's message: an icon, a Radio Canada Big title ("You're all caught up"), one muted
 * line, and an action.
 */
@Composable
fun EmptyState(
    icon: ImageVector?,
    title: String,
    message: String?,
    modifier: Modifier = Modifier,
    action: (@Composable () -> Unit)? = null,
) {
    val colors = podColors
    Column(
        modifier.fillMaxWidth().padding(horizontal = 24.dp, vertical = 72.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        if (icon != null) Icon(icon, contentDescription = null, tint = colors.faint, modifier = Modifier.size(40.dp))
        Spacer(Modifier.size(2.dp))
        Text(title, style = PodType.detailTitle, color = colors.ink, textAlign = androidx.compose.ui.text.style.TextAlign.Center)
        if (message != null) Text(message, style = PodType.body, color = colors.muted, textAlign = androidx.compose.ui.text.style.TextAlign.Center)
        if (action != null) {
            Spacer(Modifier.size(10.dp))
            action()
        }
    }
}

/** A notice about the page's subject, such as a failing feed, tinted with the danger colour. */
@Composable
fun Banner(text: String, modifier: Modifier = Modifier, tone: Color = podColors.danger) {
    val colors = podColors
    Text(
        text, style = PodType.body, color = colors.ink,
        modifier = modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(12.dp))
            .background(androidx.compose.ui.graphics.lerp(colors.surface, tone, 0.10f))
            .border(1.dp, tone.copy(alpha = 0.25f), RoundedCornerShape(12.dp))
            .padding(horizontal = 16.dp, vertical = 12.dp),
    )
}

// ==============================================================================================
// Panels (settings and other grouped rows)
// ==============================================================================================

/** A section's title and optional note over its [Panel]. */
@Composable
fun SectionTitle(title: String, note: String? = null, modifier: Modifier = Modifier) {
    Column(modifier.padding(top = 28.dp, bottom = 10.dp)) {
        Text(title, style = PodType.section, color = podColors.ink)
        if (note != null) Text(note, style = PodType.meta, color = podColors.muted, modifier = Modifier.padding(top = 2.dp))
    }
}

/** Rows of one section on one surface, with hairlines between them. */
@Composable
fun Panel(modifier: Modifier = Modifier, content: @Composable ColumnScope.() -> Unit) {
    val colors = podColors
    Column(
        modifier.fillMaxWidth().clip(RoundedCornerShape(12.dp)).background(colors.surface).border(1.dp, colors.line, RoundedCornerShape(12.dp)),
        content = content,
    )
}

/** A row of a [Panel]: a title and note, and whatever it controls at the end. */
@Composable
fun PanelRow(
    title: String,
    modifier: Modifier = Modifier,
    note: String? = null,
    first: Boolean = false,
    titleColor: Color = podColors.ink,
    onClick: (() -> Unit)? = null,
    trailing: @Composable RowScope.() -> Unit = {},
) {
    val colors = podColors
    Column(modifier) {
        if (!first) Box(Modifier.fillMaxWidth().heightIn(min = 1.dp, max = 1.dp).background(colors.line))
        Row(
            Modifier
                .fillMaxWidth()
                .then(if (onClick != null) Modifier.clickable(onClick = onClick) else Modifier)
                .heightIn(min = 56.dp)
                .padding(horizontal = 16.dp, vertical = 10.dp),
            horizontalArrangement = Arrangement.spacedBy(16.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column(Modifier.weight(1f)) {
                Text(title, style = PodType.bodyMedium, color = titleColor)
                if (note != null) Text(note, style = PodType.meta, color = colors.muted)
            }
            trailing()
        }
    }
}

/** "● All saved to the server": a dot in the accent when done, amber while waiting. */
@Composable
fun StatusDot(text: String, waiting: Boolean, modifier: Modifier = Modifier) {
    val colors = podColors
    Row(modifier, horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
        Box(Modifier.size(8.dp).clip(CircleShape).background(if (waiting) colors.warning else colors.accent))
        Text(text, style = PodType.meta, color = colors.muted, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}

// ==============================================================================================
// Fields, dialogs, toasts
// ==============================================================================================

/** A labelled single-line field: label above, 8 dp radius, the accent's ring while focused. */
@Composable
fun PodTextField(
    value: String,
    onValueChange: (String) -> Unit,
    label: String?,
    modifier: Modifier = Modifier,
    placeholder: String? = null,
    leadingIcon: ImageVector? = null,
    keyboardOptions: KeyboardOptions = KeyboardOptions.Default,
    keyboardActions: KeyboardActions = KeyboardActions.Default,
    visualTransformation: VisualTransformation = VisualTransformation.None,
    error: String? = null,
    supporting: String? = null,
    height: Dp = 46.dp,
) {
    val colors = podColors
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    val ring = when {
        error != null -> colors.danger
        focused -> colors.accent
        else -> colors.line
    }
    Column(modifier, verticalArrangement = Arrangement.spacedBy(6.dp)) {
        if (label != null) Text(label, style = PodType.metaMedium, color = colors.ink)
        BasicTextField(
            value, onValueChange,
            singleLine = true,
            textStyle = PodType.body.copy(fontSize = 16.sp, color = colors.ink),
            cursorBrush = SolidColor(colors.accent),
            keyboardOptions = keyboardOptions,
            keyboardActions = keyboardActions,
            visualTransformation = visualTransformation,
            interactionSource = interaction,
            modifier = Modifier
                .fillMaxWidth()
                .then(if (focused) Modifier.border(4.dp, ring.copy(alpha = 0.22f), RoundedCornerShape(10.dp)).padding(2.dp) else Modifier.padding(2.dp))
                .clip(RoundedCornerShape(8.dp))
                .background(colors.surface)
                .border(1.dp, ring, RoundedCornerShape(8.dp))
                .heightIn(min = height),
            decorationBox = { field ->
                Row(Modifier.padding(horizontal = 12.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                    if (leadingIcon != null) Icon(leadingIcon, contentDescription = null, tint = colors.faint, modifier = Modifier.size(20.dp))
                    Box(Modifier.weight(1f).heightIn(min = height - 4.dp), contentAlignment = Alignment.CenterStart) {
                        if (value.isEmpty() && placeholder != null) Text(placeholder, style = PodType.body.copy(fontSize = 16.sp), color = colors.faint, maxLines = 1)
                        field()
                    }
                }
            },
        )
        val below = error ?: supporting
        if (below != null) Text(below, style = PodType.meta, color = if (error != null) colors.danger else colors.muted)
    }
}

/** A confirmation: Radio Canada Big title, a muted line, Cancel and the action as pills. */
@Composable
fun PodDialog(
    title: String,
    message: String?,
    confirm: String,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
    dismiss: String = "Cancel",
    content: (@Composable () -> Unit)? = null,
) {
    val colors = podColors
    AlertDialog(
        onDismissRequest = onDismiss,
        shape = RoundedCornerShape(18.dp),
        containerColor = colors.surface,
        tonalElevation = 0.dp,
        title = { Text(title, style = PodType.detailTitle, color = colors.ink) },
        text = content ?: message?.let { { Text(it, style = PodType.body.copy(fontSize = 16.sp, lineHeight = 23.sp), color = colors.muted) } },
        dismissButton = { PodButton(dismiss, onDismiss) },
        confirmButton = { PodButton(confirm, onConfirm, kind = ButtonKind.Primary) },
    )
}

/** The snackbar as a toast: ink on the page's colour, with the action (Undo) beside it. */
@Composable
fun PodToast(data: SnackbarData) {
    val colors = podColors
    Snackbar(
        data,
        modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
        shape = RoundedCornerShape(12.dp),
        containerColor = colors.ink,
        contentColor = colors.background,
        actionColor = if (colors.dark) LightPodColors.accent else DarkPodColors.accent,
    )
}

// ==============================================================================================
// The play button with its progress ring
// ==============================================================================================

/**
 * The round play button every episode carries. A thin ring around it fills clockwise from 12
 * o'clock with how much has been heard: accent while listening, faint grey once played. A started
 * episode shows at least 6 % so it doesn't read as a stray dot. The current episode's button is
 * filled with the accent, and its ring follows the player live (the caller passes the player's
 * position for it).
 */
@Composable
fun PlayToggle(
    fraction: Float,
    played: Boolean,
    current: Boolean,
    playing: Boolean,
    description: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    size: Dp = 42.dp,
) {
    val colors = podColors
    val shown = if (played) 1f else if (fraction > 0f) fraction.coerceIn(0.06f, 1f) else 0f
    val animated by animateFloatAsState(shown, tween(400), label = "ring")
    Box(
        modifier
            .size(size)
            .clip(CircleShape)
            .background(if (current) colors.accent else Color.Transparent)
            .clickable(role = Role.Button, onClick = onClick)
            .semantics { contentDescription = description },
        contentAlignment = Alignment.Center,
    ) {
        Canvas(Modifier.size(size)) {
            val stroke = 2.dp.toPx()
            val inset = 1.dp.toPx() + stroke / 2
            val arcSize = Size(this.size.width - inset * 2, this.size.height - inset * 2)
            if (!current) drawArc(colors.line, 0f, 360f, false, Offset(inset, inset), arcSize, style = Stroke(stroke))
            if (animated > 0f) {
                val ring = when {
                    current -> colors.onAccent.copy(alpha = 0.7f)
                    played -> colors.faint
                    else -> colors.accent
                }
                drawArc(ring, -90f, 360f * animated, false, Offset(inset, inset), arcSize, style = Stroke(stroke, cap = androidx.compose.ui.graphics.StrokeCap.Round))
            }
        }
        // The triangle's weight sits left of its box; nudged right it looks centred.
        Icon(
            if (current && playing) PodIcons.Pause else PodIcons.Play,
            contentDescription = null,
            tint = if (current) colors.onAccent else colors.ink,
            modifier = Modifier.size(size * 0.43f).padding(start = if (current && playing) 0.dp else 1.5.dp),
        )
    }
}

/** Material's switch in the accent: the thumb white on the accent when on, the line colour when off. */
@Composable
fun podSwitchColors() = androidx.compose.material3.SwitchDefaults.colors(
    checkedThumbColor = podColors.onAccent,
    checkedTrackColor = podColors.accent,
    checkedBorderColor = podColors.accent,
    uncheckedThumbColor = podColors.faint,
    uncheckedTrackColor = podColors.surface,
    uncheckedBorderColor = podColors.line,
)
