import { useTranslation } from "react-i18next";
import ApiKeyInput from "../ApiKeyInput";
import type { ProviderCategory } from "@/types";

interface ApiKeySectionProps {
  id?: string;
  label?: string;
  value: string;
  onChange: (value: string) => void;
  category?: ProviderCategory;
  shouldShowLink: boolean;
  websiteUrl: string;
  placeholder?: {
    official: string;
    thirdParty: string;
  };
  disabled?: boolean;
  isPartner?: boolean;
  partnerPromotionKey?: string;
}

export function ApiKeySection({
  id,
  label,
  value,
  onChange,
  category,
  shouldShowLink,
  websiteUrl,
  placeholder,
  disabled,
  partnerPromotionKey,
}: ApiKeySectionProps) {
  const { t } = useTranslation();

  const defaultPlaceholder = {
    official: t("providerForm.officialNoApiKey", {
      defaultValue: "Official login does not require API Key, save directly",
    }),
    thirdParty: t("providerForm.apiKeyAutoFill", {
      defaultValue: "Just fill in here, config below will be auto-filled",
    }),
  };

  const finalPlaceholder = placeholder || defaultPlaceholder;

  return (
    <div className="space-y-1">
      <ApiKeyInput
        id={id}
        label={label}
        value={value}
        onChange={onChange}
        placeholder={
          category === "official"
            ? finalPlaceholder.official
            : finalPlaceholder.thirdParty
        }
        disabled={disabled ?? category === "official"}
      />
      {/* API key link */}
      {shouldShowLink && websiteUrl && (
        <div className="space-y-2 -mt-1 pl-1">
          <a
            href={websiteUrl}
            target="_blank"
            rel="noopener noreferrer"
            className="text-xs text-blue-400 dark:text-blue-500 hover:text-blue-500 dark:hover:text-blue-400 transition-colors"
          >
            {t("providerForm.getApiKey", {
              defaultValue: "Get API Key",
            })}
          </a>

          {/* Promotion display depends only on partnerPromotionKey, while
              isPartner continues to control the partner badge. */}
          {partnerPromotionKey && (
            <div className="rounded-md bg-blue-50 dark:bg-blue-950/30 p-2.5 border border-blue-200 dark:border-blue-800">
              <p className="text-xs leading-relaxed text-blue-700 dark:text-blue-300">
                💡{" "}
                {t(`providerForm.partnerPromotion.${partnerPromotionKey}`, {
                  defaultValue: "",
                })}
              </p>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
