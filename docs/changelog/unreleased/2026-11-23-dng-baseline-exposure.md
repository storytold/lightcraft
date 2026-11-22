## Camera colour

- **Fixed:** DNGs with an embedded profile that has a look table but no tone curve (Lightroom "Adobe Standard" conversions) rendered about 0.3 EV darker than Lightroom; they now get their `BaselineExposure` once more, shown as `defaultExposure` in `photo.sourceReport` (#1174).
