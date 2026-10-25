# R8 rules for the release build (minifyEnabled + shrinkResources in app/build.gradle).

# Gson reads and writes the relay DTOs by reflection on their field names: keep the classes, their fields and
# constructors exactly as declared, or the JSON keys ("type", "deskId", ...) would be renamed.
-keep class com.opendesk.app.data.dto.** { *; }
-keepattributes Signature, *Annotation*, EnclosingMethod, InnerClasses

# OkHttp ships its own consumer rules; these TLS providers are optional at runtime.
-dontwarn org.conscrypt.**
-dontwarn org.bouncycastle.**
-dontwarn org.openjsse.**
