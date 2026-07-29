import React, { useMemo } from "react";
import {
  getIcon,
  hasIcon,
  getIconMetadata,
  getIconUrl,
  isUrlIcon,
} from "@/icons/extracted";
import { cn } from "@/lib/utils";

interface ProviderIconProps {
  icon?: string; // Icon name.
  name: string; // Provider name used for the fallback.
  color?: string; // Custom color; deprecated and ignored for SVG compatibility.
  size?: number | string; // Icon size.
  className?: string;
  showFallback?: boolean; // Whether to display the fallback.
}

export const ProviderIcon: React.FC<ProviderIconProps> = ({
  icon,
  name,
  color,
  size = 32,
  className,
  showFallback = true,
}) => {
  // Resolve the inline SVG string.
  const iconSvg = useMemo(() => {
    if (icon && !isUrlIcon(icon) && hasIcon(icon)) {
      return getIcon(icon);
    }
    return "";
  }, [icon]);

  // Resolve SVG and raster image URLs listed in URL_ICONS.
  const iconUrl = useMemo(() => {
    if (icon && isUrlIcon(icon)) {
      return getIconUrl(icon);
    }
    return "";
  }, [icon]);

  // Calculate size styles.
  const sizeStyle = useMemo(() => {
    const sizeValue = typeof size === "number" ? `${size}px` : size;
    return {
      width: sizeValue,
      height: sizeValue,
      fontSize: sizeValue,
      lineHeight: 1,
    };
  }, [size]);

  // Prefer a valid supplied color, then fall back to metadata.defaultColor.
  const effectiveColor = useMemo(() => {
    if (color && typeof color === "string" && color.trim() !== "") {
      return color;
    }
    if (icon) {
      const metadata = getIconMetadata(icon);
      if (metadata?.defaultColor && metadata.defaultColor !== "currentColor") {
        return metadata.defaultColor;
      }
    }
    return undefined;
  }, [color, icon]);

  // Render inline SVG with CSS currentColor support.
  if (iconSvg) {
    return (
      <span
        className={cn(
          "inline-flex items-center justify-center flex-shrink-0",
          className,
        )}
        title={name}
        style={{ ...sizeStyle, color: effectiveColor }}
        dangerouslySetInnerHTML={{ __html: iconSvg }}
      />
    );
  }

  // Render URL-based large SVG and raster icons as an image.
  if (iconUrl) {
    return (
      <img
        src={iconUrl}
        alt={name}
        title={name}
        className={cn(
          "inline-flex items-center justify-center flex-shrink-0 object-contain",
          className,
        )}
        style={{ width: sizeStyle.width, height: sizeStyle.height }}
        loading="lazy"
      />
    );
  }

  // Fall back to initials.
  if (showFallback) {
    const initials = name
      .split(" ")
      .map((word) => word[0])
      .join("")
      .toUpperCase()
      .slice(0, 2);
    const fallbackFontSize =
      typeof size === "number" ? `${Math.max(size * 0.5, 12)}px` : "0.5em";
    return (
      <span
        className={cn(
          "inline-flex items-center justify-center flex-shrink-0 rounded-lg",
          "bg-muted text-muted-foreground font-semibold",
          className,
        )}
        title={name}
        style={sizeStyle}
      >
        <span
          style={{
            fontSize: fallbackFontSize,
          }}
        >
          {initials}
        </span>
      </span>
    );
  }

  return null;
};
