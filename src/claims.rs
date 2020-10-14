use coarsetime::{Clock, Duration, UnixTimeStamp};
use ct_codecs::{Base64UrlSafeNoPadding, Encoder};
use rand::RngCore;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::HashSet;

use crate::common::VerificationOptions;
use crate::error::*;
use crate::serde_additions;

pub const DEFAULT_TIME_TOLERANCE_SECS: u64 = 900;

/// Type representing the fact that no application-defined claims is necessary.
#[derive(Copy, Clone, Serialize, Deserialize)]
pub struct NoCustomClaims {}

/// The `audiences` property is usually an array (set), but some applications may require it to be a string.
/// We support both.
#[derive(Debug, Serialize, Deserialize, Eq, PartialEq)]
pub enum Audiences {
    AsSet(HashSet<String>),
    AsString(String),
}

impl Audiences {
    pub fn is_set(&self) -> bool {
        match self {
            Audiences::AsSet(_) => true,
            _ => false,
        }
    }

    pub fn is_string(&self) -> bool {
        return !self.is_set();
    }
}

/// A set of JWT claims.
///
/// The `CustomClaims` parameter can be set to `NoCustomClaims` if only standard claims are used,
/// or to a user-defined type that must be `serde`-serializable if custom claims are required.
#[derive(Debug, Serialize, Deserialize)]
pub struct JWTClaims<CustomClaims> {
    /// Time the claims were created at
    #[serde(
        rename = "iat",
        default,
        skip_serializing_if = "Option::is_none",
        with = "self::serde_additions::unix_timestamp"
    )]
    pub issued_at: Option<UnixTimeStamp>,

    /// Time the claims expire at
    #[serde(
        rename = "exp",
        default,
        skip_serializing_if = "Option::is_none",
        with = "self::serde_additions::unix_timestamp"
    )]
    pub expires_at: Option<UnixTimeStamp>,

    /// Time the claims will be invalid until
    #[serde(
        rename = "nbf",
        default,
        skip_serializing_if = "Option::is_none",
        with = "self::serde_additions::unix_timestamp"
    )]
    pub invalid_before: Option<UnixTimeStamp>,

    /// Issuer - This can be set to anything application-specific
    #[serde(rename = "iss", default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,

    /// Subject - This can be set to anything application-specific
    #[serde(rename = "sub", default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,

    /// Audience
    #[serde(rename = "aud", default, skip_serializing_if = "Option::is_none")]
    pub audiences: Option<Audiences>,

    /// The audience should be a set, but some applications require a string instead.
    #[serde(skip, default)]
    audiences_as_string: bool,

    /// JWT identifier
    ///
    /// That property was originally designed to avoid replay attacks, but keeping
    /// all previously sent JWT token IDs is unrealistic.
    ///
    /// Replay attacks are better addressed by keeping only the timestamp of the last
    /// valid token for a user, and rejecting anything older in future tokens.
    #[serde(rename = "jti", default, skip_serializing_if = "Option::is_none")]
    pub jwt_id: Option<String>,

    /// Nonce
    #[serde(rename = "nonce", default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,

    /// Custom (application-defined) claims
    #[serde(flatten)]
    pub custom: CustomClaims,
}

impl<CustomClaims> JWTClaims<CustomClaims> {
    pub(crate) fn validate(&self, options: &VerificationOptions) -> Result<(), Error> {
        let now = Clock::now_since_epoch();
        let time_tolerance = options
            .time_tolerance
            .unwrap_or_else(|| Duration::from_secs(DEFAULT_TIME_TOLERANCE_SECS));

        if let Some(reject_before) = options.reject_before {
            ensure!(now <= reject_before, JWTError::OldTokenReused);
        }
        if let Some(time_issued) = self.issued_at {
            ensure!(time_issued <= now + time_tolerance, JWTError::ClockDrift);
            if let Some(max_validity) = options.max_validity {
                ensure!(
                    now <= time_issued || now - time_issued <= max_validity,
                    JWTError::TokenIsTooOld
                );
            }
        }
        if !options.accept_future {
            if let Some(invalid_before) = self.invalid_before {
                ensure!(now >= invalid_before, JWTError::TokenNotValidYet);
            }
        }
        if let Some(expires_at) = self.expires_at {
            ensure!(
                now - time_tolerance <= expires_at,
                JWTError::TokenHasExpired
            );
        }
        if let Some(required_issuer) = &options.required_issuer {
            if let Some(issuer) = &self.issuer {
                ensure!(issuer == required_issuer, JWTError::RequiredIssuerMismatch);
            } else {
                bail!(JWTError::RequiredIssuerMissing);
            }
        }
        if let Some(required_subject) = &options.required_subject {
            if let Some(subject) = &self.subject {
                ensure!(
                    subject == required_subject,
                    JWTError::RequiredSubjectMismatch
                );
            } else {
                bail!(JWTError::RequiredSubjectMissing);
            }
        }
        if let Some(required_nonce) = &options.required_nonce {
            if let Some(nonce) = &self.nonce {
                ensure!(nonce == required_nonce, JWTError::RequiredNonceMismatch);
            } else {
                bail!(JWTError::RequiredNonceMissing);
            }
        }
        if let Some(required_audiences) = &options.required_audiences {
            if let Some(audiences) = &self.audiences {
                let mut single_audience;
                let audiences = match audiences {
                    Audiences::AsString(audience) => {
                        single_audience = HashSet::new();
                        single_audience.insert(audience.to_string());
                        &single_audience
                    }
                    Audiences::AsSet(audiences) => audiences,
                };
                for required_audience in required_audiences {
                    ensure!(
                        audiences.contains(required_audience),
                        JWTError::RequiredAudiencesMismatch
                    )
                }
            } else if !required_audiences.is_empty() {
                bail!(JWTError::RequiredAudiencesMissing);
            }
        }
        Ok(())
    }

    /// Set the token as not being valid until `unix_timestamp`
    pub fn invalid_before(mut self, unix_timestamp: UnixTimeStamp) -> Self {
        self.invalid_before = Some(unix_timestamp);
        self
    }

    /// Set the issuer
    pub fn with_issuer(mut self, issuer: impl ToString) -> Self {
        self.issuer = Some(issuer.to_string());
        self
    }

    /// Set the subject
    pub fn with_subject(mut self, subject: impl ToString) -> Self {
        self.subject = Some(subject.to_string());
        self
    }

    fn convert_audiences_format(&mut self) -> Result<(), Error> {
        let audiences = self.audiences.as_ref();
        let updated_audiences;
        if self.audiences_as_string {
            // convert audiences to a string
            match audiences {
                Some(Audiences::AsString(_)) | None => return Ok(()),
                Some(Audiences::AsSet(audiences)) => {
                    if audiences.len() > 1 {
                        bail!(JWTError::TooManyAudiences);
                    }
                    updated_audiences = Some(Audiences::AsString(
                        audiences
                            .iter()
                            .next()
                            .map(|x| x.to_string())
                            .unwrap_or_default(),
                    ));
                }
            }
        } else {
            // convert audiences to a set
            match audiences {
                Some(Audiences::AsSet(_)) | None => return Ok(()),
                Some(Audiences::AsString(audiences)) => {
                    let mut audiences_set = HashSet::new();
                    if !audiences.is_empty() {
                        audiences_set.insert(audiences.to_string());
                    }
                    updated_audiences = Some(Audiences::AsSet(audiences_set));
                }
            }
        }
        self.audiences = updated_audiences;
        Ok(())
    }

    /// The audiences should be a set (and this is the default), but some applications expect a string instead.
    /// Call this function in order to create a token where the audiences will be serialized as a string.
    /// If this is the case, no more than one element is allowed (but it can includes commas, or whatever delimiter the application expects).
    pub fn audiences_as_string(mut self, serialize_as_string: bool) -> Result<Self, Error> {
        self.audiences_as_string = serialize_as_string;
        self.convert_audiences_format()?;
        Ok(self)
    }

    /// Set the audiences
    pub fn with_audiences(mut self, audiences: HashSet<impl ToString>) -> Result<Self, Error> {
        self.audiences = Some(Audiences::AsSet(
            audiences.iter().map(|x| x.to_string()).collect(),
        ));
        self.convert_audiences_format()?;
        Ok(self)
    }

    /// Set the JWT identifier
    pub fn with_jwt_id(mut self, jwt_id: impl ToString) -> Self {
        self.jwt_id = Some(jwt_id.to_string());
        self
    }

    /// Set the nonce
    pub fn with_nonce(mut self, nonce: impl ToString) -> Self {
        self.nonce = Some(nonce.to_string());
        self
    }

    /// Create a nonce, attach it and return it
    pub fn create_nonce(&mut self) -> &str {
        let mut raw_nonce = [0u8; 24];
        let mut rng = rand::thread_rng();
        rng.fill_bytes(&mut raw_nonce);
        let nonce = Base64UrlSafeNoPadding::encode_to_string(raw_nonce).unwrap();
        self.nonce = Some(nonce);
        &self.nonce.as_deref().unwrap()
    }
}

pub struct Claims;

impl Claims {
    /// Create a new set of claims, without custom data, expiring in `valid_for`.
    pub fn create(valid_for: Duration) -> JWTClaims<NoCustomClaims> {
        let now = Some(Clock::now_since_epoch());
        JWTClaims {
            issued_at: now,
            expires_at: Some(now.unwrap() + valid_for),
            invalid_before: now,
            audiences: None,
            audiences_as_string: false,
            issuer: None,
            jwt_id: None,
            subject: None,
            nonce: None,
            custom: NoCustomClaims {},
        }
    }

    /// Create a new set of claims, with custom data, expiring in `valid_for`.
    pub fn with_custom_claims<CustomClaims: Serialize + DeserializeOwned>(
        custom_claims: CustomClaims,
        valid_for: Duration,
    ) -> JWTClaims<CustomClaims> {
        let now = Some(Clock::now_since_epoch());
        JWTClaims {
            issued_at: now,
            expires_at: Some(now.unwrap() + valid_for),
            invalid_before: now,
            audiences: None,
            audiences_as_string: false,
            issuer: None,
            jwt_id: None,
            subject: None,
            nonce: None,
            custom: custom_claims,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_set_standard_claims() {
        let exp = Duration::from_mins(10);
        let mut audiences = HashSet::new();
        audiences.insert("audience1".to_string());
        audiences.insert("audience2".to_string());
        let claims = Claims::create(exp)
            .with_audiences(audiences.clone())
            .unwrap()
            .with_issuer("issuer")
            .with_jwt_id("jwt_id")
            .with_nonce("nonce")
            .with_subject("subject");

        assert_eq!(claims.audiences, Some(Audiences::AsSet(audiences)));
        assert_eq!(claims.issuer, Some("issuer".to_owned()));
        assert_eq!(claims.jwt_id, Some("jwt_id".to_owned()));
        assert_eq!(claims.nonce, Some("nonce".to_owned()));
        assert_eq!(claims.subject, Some("subject".to_owned()));
    }
}
