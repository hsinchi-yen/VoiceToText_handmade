package app.v2c.ime.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Shapes
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/*
 * v2c design tokens for Android. Colors, corner radii and type scale mirror the desktop app
 * (apps/desktop/ui/app.css, "Material 3 Expressive style, own blue-violet palette"), so the
 * phone keyboard, the setup screen and the Windows/Linux app look like one product.
 * Keep the two in sync when either changes.
 */

private val LightColors = lightColorScheme(
    primary = Color(0xFF5A4FCF),
    onPrimary = Color(0xFFFFFFFF),
    primaryContainer = Color(0xFFE3DFFF),
    onPrimaryContainer = Color(0xFF16095F),
    secondary = Color(0xFF5E5C71),
    onSecondary = Color(0xFFFFFFFF),
    secondaryContainer = Color(0xFFE4E0F4),
    onSecondaryContainer = Color(0xFF1B1A2C),
    // Accent from the app icon's yellow "V": used by the 中→英 key.
    tertiary = Color(0xFF7A5900),
    onTertiary = Color(0xFFFFFFFF),
    tertiaryContainer = Color(0xFFFFE3B0),
    onTertiaryContainer = Color(0xFF3D2A00),
    background = Color(0xFFFCF8FF),
    onBackground = Color(0xFF1C1B20),
    surface = Color(0xFFFCF8FF),
    onSurface = Color(0xFF1C1B20),
    surfaceVariant = Color(0xFFE4E1EA),
    onSurfaceVariant = Color(0xFF47464F),
    surfaceContainerLowest = Color(0xFFFFFFFF),
    surfaceContainerLow = Color(0xFFF6F2FB),
    surfaceContainer = Color(0xFFF0ECF6),
    surfaceContainerHigh = Color(0xFFEAE6F0),
    surfaceContainerHighest = Color(0xFFE4E1EA),
    outline = Color(0xFF78767F),
    outlineVariant = Color(0xFFC8C5D0),
    error = Color(0xFFBA1A1A),
    onError = Color(0xFFFFFFFF),
    errorContainer = Color(0xFFFFDAD6),
    onErrorContainer = Color(0xFF410002),
    scrim = Color(0xFF000000),
)

private val DarkColors = darkColorScheme(
    primary = Color(0xFFC6C0FF),
    onPrimary = Color(0xFF2A1B9A),
    primaryContainer = Color(0xFF41369F),
    onPrimaryContainer = Color(0xFFE3DFFF),
    secondary = Color(0xFFC8C4DC),
    onSecondary = Color(0xFF302E41),
    secondaryContainer = Color(0xFF4A4760),
    onSecondaryContainer = Color(0xFFE4E0F4),
    tertiary = Color(0xFFF0BF5F),
    onTertiary = Color(0xFF412D00),
    tertiaryContainer = Color(0xFF5D4200),
    onTertiaryContainer = Color(0xFFFFDEA6),
    background = Color(0xFF141318),
    onBackground = Color(0xFFE5E1E9),
    surface = Color(0xFF141318),
    onSurface = Color(0xFFE5E1E9),
    surfaceVariant = Color(0xFF36343A),
    onSurfaceVariant = Color(0xFFC8C5D0),
    surfaceContainerLowest = Color(0xFF0F0E13),
    surfaceContainerLow = Color(0xFF1C1B20),
    surfaceContainer = Color(0xFF201F25),
    surfaceContainerHigh = Color(0xFF2B292F),
    surfaceContainerHighest = Color(0xFF36343A),
    outline = Color(0xFF928F99),
    outlineVariant = Color(0xFF47464F),
    error = Color(0xFFFFB4AB),
    onError = Color(0xFF690005),
    errorContainer = Color(0xFF93000A),
    onErrorContainer = Color(0xFFFFDAD6),
    scrim = Color(0xFF000000),
)

/** Status colors the Material scheme has no slot for (desktop `--md-success` / `--md-warning`). */
@Immutable
data class V2cStatusColors(
    val success: Color,
    val successContainer: Color,
    val warning: Color,
    val warningContainer: Color,
)

private val LightStatus = V2cStatusColors(
    success = Color(0xFF1B6E3A),
    successContainer = Color(0xFFC8F0D2),
    warning = Color(0xFF8A5A00),
    warningContainer = Color(0xFFFFE3B0),
)

private val DarkStatus = V2cStatusColors(
    success = Color(0xFF7BD89A),
    successContainer = Color(0xFF0B3D1E),
    warning = Color(0xFFF0BF5F),
    warningContainer = Color(0xFF4A3300),
)

val LocalV2cStatus = staticCompositionLocalOf { LightStatus }

/** Desktop radii: xs 4, sm 8, md 12, lg 16, xl 28 (buttons are pills). */
private val V2cShapes = Shapes(
    extraSmall = RoundedCornerShape(4.dp),
    small = RoundedCornerShape(8.dp),
    medium = RoundedCornerShape(12.dp),
    large = RoundedCornerShape(16.dp),
    extraLarge = RoundedCornerShape(28.dp),
)

private fun style(size: Int, line: Int, weight: FontWeight) =
    TextStyle(fontSize = size.sp, lineHeight = line.sp, fontWeight = weight)

/** Desktop type scale (display 36, headline 26, title 18/15, body 15/13, label 14/12). */
private val V2cTypography = Typography().let { base ->
    base.copy(
        displaySmall = base.displaySmall.merge(style(36, 44, FontWeight.SemiBold)),
        headlineMedium = base.headlineMedium.merge(style(26, 34, FontWeight.SemiBold)),
        headlineSmall = base.headlineSmall.merge(style(22, 30, FontWeight.SemiBold)),
        titleLarge = base.titleLarge.merge(style(18, 26, FontWeight.SemiBold)),
        titleMedium = base.titleMedium.merge(style(15, 22, FontWeight.SemiBold)),
        titleSmall = base.titleSmall.merge(style(14, 20, FontWeight.SemiBold)),
        bodyLarge = base.bodyLarge.merge(style(15, 22, FontWeight.Normal)),
        bodyMedium = base.bodyMedium.merge(style(14, 20, FontWeight.Normal)),
        bodySmall = base.bodySmall.merge(style(13, 19, FontWeight.Normal)),
        labelLarge = base.labelLarge.merge(style(14, 20, FontWeight.SemiBold)),
        labelMedium = base.labelMedium.merge(style(12, 16, FontWeight.SemiBold)),
        labelSmall = base.labelSmall.merge(style(11, 16, FontWeight.Medium)),
    )
}

/** The v2c theme: follows the system light/dark setting, like the desktop app. */
@Composable
fun V2cTheme(dark: Boolean = isSystemInDarkTheme(), content: @Composable () -> Unit) {
    val colors: ColorScheme = if (dark) DarkColors else LightColors
    CompositionLocalProvider(LocalV2cStatus provides if (dark) DarkStatus else LightStatus) {
        MaterialTheme(
            colorScheme = colors,
            shapes = V2cShapes,
            typography = V2cTypography,
            content = content,
        )
    }
}
